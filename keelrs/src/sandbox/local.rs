//! [`local()`]: this machine as a sandbox. Files go through rig-tools'
//! [`Local`] workspace (ADR 0004, rule 2) on a blocking thread, and Rig
//! types stay in this module. Commands run on `tokio::process`, each in a
//! session of its own.

#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // the one place keelrs touches the host

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures::SinkExt as _;
use futures::channel::{mpsc, oneshot};
use process_wrap::tokio::{ChildWrapper, CommandWrap, ProcessSession};
use rig_tools::workspace::{Listing, Local, Workspace as _};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};
use tokio::net::unix::pipe;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
    Chunk, Command, ContentHash, Contents, ExecRequest, Execution, Exit, Sandbox, SandboxBackend,
    SandboxError, SandboxPath, Stat,
};
use crate::Result;

/// The host variables a command starts from; `LC_*` too.
const ENV_ALLOWLIST: [&str; 10] = [
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TZ", "TERM", "TMPDIR", "LC_ALL",
];

/// How long a stopped command has between SIGTERM and SIGKILL.
const GRACE: Duration = Duration::from_secs(2);

/// How long what a command left in its group may hold its output open.
const LEFTOVER: Duration = Duration::from_millis(100);

/// The longest piece of output sent at once, when a line runs longer.
const CHUNK: usize = 64 * 1024;

/// How many pieces of output wait for a reader before the command blocks.
const QUEUED_CHUNKS: usize = 64;

/// Starts a [`LocalBuilder`]: this machine as a sandbox.
///
/// ```no_run
/// # async fn f() -> keelrs::Result<()> {
/// let sb = keelrs::sandbox::local()
///     .root("/home/me/repo")
///     .env("GRADLE_USER_HOME", "/home/me/.gradle-keelrs")
///     .open()
///     .await?;
/// # Ok(()) }
/// ```
#[must_use]
pub fn local() -> LocalBuilder {
    LocalBuilder::default()
}

/// Opens this machine as a sandbox: its root directory and the variables
/// its commands get.
#[derive(Debug, Default)]
pub struct LocalBuilder {
    root: Option<PathBuf>,
    env: Vec<(String, Option<String>)>,
}

impl LocalBuilder {
    /// The sandbox's root, the root handle's work dir; by default this
    /// process's working directory.
    #[must_use]
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Sets `name` to `value` for every command, over the allowlisted host
    /// variables (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`,
    /// `LC_*`, `TZ`, `TERM`, `TMPDIR`) and the default
    /// `GIT_TERMINAL_PROMPT=0`.
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), Some(value.into())));
        self
    }

    /// Removes `name` from every command's variables.
    #[must_use]
    pub fn env_remove(mut self, name: impl Into<String>) -> Self {
        self.env.push((name.into(), None));
        self
    }

    /// The sandbox, rooted at the root's canonical path, which must be a
    /// directory. The host's variables are read here and never again.
    pub async fn open(self) -> Result<Sandbox> {
        let root = self.root.unwrap_or_else(|| PathBuf::from("."));
        let shown = root.display().to_string();
        let (root, shell) = blocking("open", shown.clone(), move || {
            let root = Local.canonical(&root);
            if Local.stat(&root)? == rig_tools::workspace::Stat::Dir {
                Ok((root, find_shell()))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "not a directory",
                ))
            }
        })
        .await?;
        let root = sandbox_path("open", &root)?;
        let shell = sandbox_path("open", &shell)?;
        let mut env: Vec<(String, String)> = std::env::vars()
            .filter(|(name, _)| ENV_ALLOWLIST.contains(&name.as_str()) || name.starts_with("LC_"))
            .collect();
        env.push(("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()));
        apply(&mut env, self.env);
        Ok(Sandbox::new(LocalBackend { env, shell }, root))
    }
}

/// This machine, as [`Sandbox`] reaches it.
struct LocalBackend {
    /// What every command starts from, read once at `open`.
    env: Vec<(String, String)>,
    /// What runs [`Command::Shell`] lines, found at `open`.
    shell: SandboxPath,
}

#[async_trait::async_trait]
impl SandboxBackend for LocalBackend {
    async fn stat(&self, path: &SandboxPath) -> Result<Stat> {
        let at = host(path);
        blocking("stat", path.to_string(), move || {
            Ok(match Local.stat(&at)? {
                rig_tools::workspace::Stat::File {
                    size,
                    read_only,
                    modified,
                } => Stat::File {
                    size,
                    read_only,
                    modified,
                },
                rig_tools::workspace::Stat::Dir => Stat::Dir,
                rig_tools::workspace::Stat::Other => Stat::Other,
                rig_tools::workspace::Stat::Missing => Stat::Missing,
            })
        })
        .await
    }

    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents> {
        let at = host(path);
        blocking("read", path.to_string(), move || {
            let read = Local.read(&at, limit)?;
            Ok(Contents {
                bytes: read.bytes,
                size: read.size,
            })
        })
        .await
    }

    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()> {
        let at = host(path);
        blocking("write", path.to_string(), move || Local.write(&at, &bytes)).await
    }

    async fn write_if(
        &self,
        path: &SandboxPath,
        bytes: Vec<u8>,
        held: ContentHash,
    ) -> Result<Option<Contents>> {
        let at = host(path);
        let held = rig_core::tool::ContentHash(held.0);
        blocking("write_if", path.to_string(), move || {
            let now = Local.write_if(&at, &bytes, &held)?;
            Ok(now.map(|now| Contents {
                bytes: now.bytes,
                size: now.size,
            }))
        })
        .await
    }

    async fn remove(&self, path: &SandboxPath, recursive: bool) -> Result<()> {
        let at = host(path);
        blocking("remove", path.to_string(), move || {
            match Local.stat(&at)? {
                rig_tools::workspace::Stat::Dir if recursive => std::fs::remove_dir_all(&at),
                rig_tools::workspace::Stat::Dir => std::fs::remove_dir(&at),
                _ => Local.remove(&at),
            }
        })
        .await
    }

    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()> {
        let (source, target) = (host(from), host(to));
        blocking("rename", from.to_string(), move || {
            Local.rename(&source, &target)
        })
        .await
    }

    async fn list(&self, root: &SandboxPath, glob: Option<&str>) -> Result<Vec<SandboxPath>> {
        let at = host(root);
        let glob = glob.map(str::to_owned);
        let files = blocking("list", root.to_string(), move || {
            Local.files(&at, glob.as_deref(), Listing::All)
        })
        .await?;
        // A name that is not UTF-8 has no `SandboxPath`, so it is left out.
        Ok(files
            .iter()
            .filter_map(|file| sandbox_path("list", file).ok())
            .collect())
    }

    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        let at = host(path);
        let canonical = blocking("canonical", path.to_string(), move || {
            Ok(Local.canonical(&dangling_target(Local.canonical(&at))))
        })
        .await?;
        sandbox_path("canonical", &canonical)
    }

    async fn read_files(&self, paths: &[SandboxPath], limit: u64) -> Vec<Option<Vec<u8>>> {
        let at: Vec<PathBuf> = paths.iter().map(host).collect();
        let count = paths.len();
        blocking("read_files", String::new(), move || {
            Ok(Local.read_files(&at, limit))
        })
        .await
        .unwrap_or_else(|_| vec![None; count])
    }

    fn shell(&self) -> Option<&SandboxPath> {
        Some(&self.shell)
    }

    async fn exec(&self, request: ExecRequest) -> Result<Execution> {
        let (program, args) = match request.command {
            Command::Shell(line) => (self.shell.to_string(), vec!["-c".to_owned(), line]),
            Command::Argv(argv) => {
                let mut argv = argv.into_iter();
                let Some(program) = argv.next() else {
                    return Err(SandboxError::Io {
                        operation: "exec",
                        path: String::new(),
                        kind: io::ErrorKind::InvalidInput,
                        message: "no program to run".to_owned(),
                    }
                    .into());
                };
                (program, argv.collect())
            }
        };
        let mut vars = self.env.clone();
        apply(&mut vars, request.env);
        let shown = program.clone();
        let failed = move |e: io::Error| io_error("exec", shown.clone(), &e);
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&args)
            .current_dir(request.cwd.unwrap_or_else(|| "/".to_owned()))
            .env_clear()
            .envs(vars)
            .stdin(if request.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        // Both streams on one pipe keep their order.
        let merged = if request.merge_stderr {
            let (reader, writer) = io::pipe().map_err(&failed)?;
            command
                .stderr(writer.try_clone().map_err(&failed)?)
                .stdout(writer);
            Some(pipe::Receiver::from_owned_fd(reader.into()).map_err(&failed)?)
        } else {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            None
        };
        let mut command = CommandWrap::from(command);
        command.wrap(ProcessSession);
        let mut child = command.spawn().map_err(&failed)?;
        drop(command); // closes our end of a merged pipe

        let (send, output) = mpsc::channel(QUEUED_CHUNKS);
        let cut = CancellationToken::new();
        let mut pumps = Vec::new();
        if let Some(reader) = merged {
            pumps.push(tokio::spawn(pump(
                reader,
                Chunk::Out,
                send.clone(),
                cut.clone(),
            )));
        }
        if let Some(stdout) = child.stdout().take() {
            pumps.push(tokio::spawn(pump(
                stdout,
                Chunk::Out,
                send.clone(),
                cut.clone(),
            )));
        }
        if let Some(stderr) = child.stderr().take() {
            pumps.push(tokio::spawn(pump(stderr, Chunk::Err, send, cut.clone())));
        }
        if let (Some(mut stdin), Some(bytes)) = (child.stdin().take(), request.stdin) {
            tokio::spawn(async move { stdin.write_all(&bytes).await.ok() });
        }

        let (stop, stopped) = oneshot::channel();
        let (send_exit, exit) = oneshot::channel();
        tokio::spawn(async move {
            let exit = drive(child, pumps, cut, request.timeout, stopped).await;
            send_exit.send(exit.map_err(failed)).ok();
        });
        let exit = async move {
            exit.await
                .unwrap_or_else(|_| Err(SandboxError::Died { operation: "exec" }.into()))
        };
        Ok(Execution::new(output, exit, move || {
            stop.send(()).ok();
        }))
    }
}

/// Sets (`Some`) or removes (`None`) each variable in `changes`, in order.
fn apply(vars: &mut Vec<(String, String)>, changes: Vec<(String, Option<String>)>) {
    for (name, value) in changes {
        vars.retain(|(other, _)| *other != name);
        if let Some(value) = value {
            vars.push((name, value));
        }
    }
}

/// Waits for `child` to end, or stops it at its `timeout` or once `stopped`:
/// SIGTERM to its group, then SIGKILL [`GRACE`] later or once it exited.
/// Whatever is left in its group then has [`LEFTOVER`] to close the pipes
/// before it is killed and the pumps are `cut`. A stop that comes while it
/// ends still ends it as [`Exit::Killed`].
async fn drive(
    child: Box<dyn ChildWrapper>,
    pumps: Vec<JoinHandle<()>>,
    cut: CancellationToken,
    timeout: Option<Duration>,
    mut stopped: oneshot::Receiver<()>,
) -> io::Result<Exit> {
    let mut group = Group {
        child,
        seen_through: false,
    };
    let ended = tokio::select! {
        status = group.child.wait() => Ok(status),
        () = tokio::time::sleep(timeout.unwrap_or(Duration::MAX)) => Err(Exit::TimedOut),
        _ = &mut stopped => Err(Exit::Killed),
    };
    // Signals fail once the group is gone, which is what they are for.
    let mut exit = match ended {
        Ok(Ok(status)) => exit_of(status),
        Ok(Err(e)) => {
            group.child.start_kill().ok();
            cut.cancel();
            return Err(e);
        }
        Err(exit) => {
            group.child.signal(libc::SIGTERM).ok();
            tokio::time::timeout(GRACE, group.child.wait()).await.ok();
            group.child.start_kill().ok();
            group.child.wait().await.ok();
            exit
        }
    };
    let mut pumps = futures::future::join_all(pumps);
    if tokio::time::timeout(LEFTOVER, &mut pumps).await.is_err() {
        group.child.start_kill().ok();
        cut.cancel();
        pumps.await;
    }
    if stopped.try_recv() == Ok(Some(())) {
        exit = Exit::Killed;
    }
    group.seen_through = true;
    Ok(exit)
}

/// A command's process group, killed when dropped before [`drive`] saw it
/// through, as when the runtime it ran on shuts down.
struct Group {
    child: Box<dyn ChildWrapper>,
    seen_through: bool,
}

impl Drop for Group {
    fn drop(&mut self) {
        if !self.seen_through {
            self.child.start_kill().ok();
        }
    }
}

/// Where `path` leads when it is a dangling link, which `Local.canonical`
/// leaves as it is: the end of its chain of links, where a write creates
/// the file. Any other path as it is.
fn dangling_target(path: PathBuf) -> PathBuf {
    /// Linux's own limit on links followed in one lookup.
    const MAX_HOPS: usize = 40;
    let mut target = path;
    for _ in 0..MAX_HOPS {
        let Ok(link) = std::fs::read_link(&target) else {
            break;
        };
        target = target.parent().unwrap_or(Path::new("/")).join(link);
    }
    target
}

/// Sends what `pipe` prints in whole lines, all those one read brings, or
/// at most [`CHUNK`] bytes of a longer line, at a time.
/// Once nobody reads, it still drains the pipe so the command never blocks;
/// once `cut`, it stops at the first read that would wait.
async fn pump(
    pipe: impl AsyncRead + Unpin,
    chunk: fn(String) -> Chunk,
    mut send: mpsc::Sender<Chunk>,
    cut: CancellationToken,
) {
    let mut pipe = BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        let read = tokio::select! {
            biased;
            read = pipe.fill_buf() => read.unwrap_or_default(),
            () = cut.cancelled() => &[],
        };
        let done = read.is_empty();
        let (len, ended) = read
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or((read.len(), false), |at| (at + 1, true));
        line.extend_from_slice(&read[..len]);
        pipe.consume(len);
        if (ended || done || line.len() >= CHUNK) && !line.is_empty() {
            // A line split for length keeps a character cut in two for the next piece.
            let whole = match std::str::from_utf8(&line) {
                Err(e) if !(ended || done) && e.error_len().is_none() => e.valid_up_to(),
                _ => line.len(),
            };
            let text = String::from_utf8_lossy(&line[..whole]).into_owned();
            line.drain(..whole);
            send.send(chunk(text)).await.ok();
        }
        if done {
            return;
        }
    }
}

/// The SHA-256 of `bytes` as lowercase hex, as rig-tools hashes what `write_if` compares, so
/// [`ContentHash`] stays in step with it.
pub(super) fn content_hash(bytes: &[u8]) -> String {
    rig_core::tool::content_hash(bytes).0
}

/// How a process that exited ended: its code, or the signal that ended it.
fn exit_of(status: std::process::ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => Exit::Code(code),
        (None, Some(signal)) => Exit::Signaled(signal),
        (None, None) => Exit::Code(-1),
    }
}

/// `bash` on the host's `PATH`, else `sh`, by absolute path; `/bin/sh`
/// when neither is there.
fn find_shell() -> PathBuf {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    ["bash", "sh"]
        .into_iter()
        .find_map(|name| {
            dirs.iter()
                .map(|dir| dir.join(name))
                .find(|at| at.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// `path` as this machine names it.
fn host(path: &SandboxPath) -> PathBuf {
    PathBuf::from(path.as_str())
}

/// `path` as a [`SandboxPath`], refused when it is not UTF-8.
fn sandbox_path(operation: &'static str, path: &Path) -> Result<SandboxPath> {
    path.to_str().and_then(SandboxPath::new).ok_or_else(|| {
        SandboxError::Io {
            operation,
            path: path.display().to_string(),
            kind: io::ErrorKind::InvalidData,
            message: "the path is not absolute UTF-8".to_owned(),
        }
        .into()
    })
}

/// Runs `work` on a blocking thread; a panic there is [`SandboxError::Died`].
async fn blocking<T: Send + 'static>(
    operation: &'static str,
    path: String,
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done.map_err(|e| io_error(operation, path, &e)),
        Err(_) => Err(SandboxError::Died { operation }.into()),
    }
}

fn io_error(operation: &'static str, path: String, error: &io::Error) -> crate::Error {
    SandboxError::Io {
        operation,
        path,
        kind: error.kind(),
        message: error.to_string(),
    }
    .into()
}
