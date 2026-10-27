//! [`local()`]: this machine as a sandbox, on rig-tools' [`Local`]
//! workspace and [`Launcher`] (ADR 0004, rule 2). Rig types stay in this
//! module; each call runs on a blocking thread.

#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // the one place keelrs touches the host

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use futures::channel::{mpsc, oneshot};
use futures::future::BoxFuture;
use rig_tools::process::{Ended, KillOnDrop, Launcher, OutputLine, Spawn};
use rig_tools::workspace::{Listing, Local, Workspace as _};

use super::{
    Chunk, Command, ContentHash, Contents, ExecRequest, Execution, Exit, Sandbox, SandboxBackend,
    SandboxError, SandboxPath, Stat,
};
use crate::Result;

/// The host variables a command starts from; `LC_*` too.
const ENV_ALLOWLIST: [&str; 10] = [
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TZ", "TERM", "TMPDIR", "LC_ALL",
];

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
    env: Vec<(String, String)>,
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
    /// `LC_*`, `TZ`, `TERM`, `TMPDIR`).
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// The sandbox, rooted at the root's canonical path, which must be a
    /// directory. The host's variables are read here and never again.
    pub async fn open(self) -> Result<Sandbox> {
        let root = self.root.unwrap_or_else(|| PathBuf::from("."));
        let shown = root.display().to_string();
        let root = blocking("open", shown.clone(), move || {
            let root = Local.canonical(&root);
            if Local.stat(&root)? == rig_tools::workspace::Stat::Dir {
                Ok(root)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "not a directory",
                ))
            }
        })
        .await?;
        let root = sandbox_path("open", &root)?;
        let mut env: Vec<(String, String)> = std::env::vars()
            .filter(|(name, _)| ENV_ALLOWLIST.contains(&name.as_str()) || name.starts_with("LC_"))
            .collect();
        for (name, value) in self.env {
            env.retain(|(other, _)| *other != name);
            env.push((name, value));
        }
        Ok(Sandbox::new(LocalBackend { env: Arc::new(env) }, root))
    }
}

/// This machine, as [`Sandbox`] reaches it.
struct LocalBackend {
    /// What every command starts from, read once at `open`.
    env: Arc<Vec<(String, String)>>,
}

impl SandboxBackend for LocalBackend {
    fn stat<'a>(&'a self, path: &'a SandboxPath) -> BoxFuture<'a, Result<Stat>> {
        let at = host(path);
        Box::pin(blocking("stat", path.to_string(), move || {
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
        }))
    }

    fn read<'a>(&'a self, path: &'a SandboxPath, limit: u64) -> BoxFuture<'a, Result<Contents>> {
        let at = host(path);
        Box::pin(blocking("read", path.to_string(), move || {
            let read = Local.read(&at, limit)?;
            Ok(Contents {
                bytes: read.bytes,
                size: read.size,
            })
        }))
    }

    fn write<'a>(&'a self, path: &'a SandboxPath, bytes: Vec<u8>) -> BoxFuture<'a, Result<()>> {
        let at = host(path);
        Box::pin(blocking("write", path.to_string(), move || {
            Local.write(&at, &bytes)
        }))
    }

    fn write_if<'a>(
        &'a self,
        path: &'a SandboxPath,
        bytes: Vec<u8>,
        held: ContentHash,
    ) -> BoxFuture<'a, Result<Option<Contents>>> {
        let at = host(path);
        let held = rig_core::tool::ContentHash(held.0);
        Box::pin(blocking("write_if", path.to_string(), move || {
            let now = Local.write_if(&at, &bytes, &held)?;
            Ok(now.map(|now| Contents {
                bytes: now.bytes,
                size: now.size,
            }))
        }))
    }

    fn remove<'a>(&'a self, path: &'a SandboxPath, recursive: bool) -> BoxFuture<'a, Result<()>> {
        let at = host(path);
        Box::pin(blocking("remove", path.to_string(), move || {
            match Local.stat(&at)? {
                rig_tools::workspace::Stat::Dir if recursive => std::fs::remove_dir_all(&at),
                rig_tools::workspace::Stat::Dir => std::fs::remove_dir(&at),
                _ => Local.remove(&at),
            }
        }))
    }

    fn rename<'a>(
        &'a self,
        from: &'a SandboxPath,
        to: &'a SandboxPath,
    ) -> BoxFuture<'a, Result<()>> {
        let (source, target) = (host(from), host(to));
        Box::pin(blocking("rename", from.to_string(), move || {
            Local.rename(&source, &target)
        }))
    }

    fn list<'a>(
        &'a self,
        root: &'a SandboxPath,
        glob: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<SandboxPath>>> {
        let at = host(root);
        let glob = glob.map(str::to_owned);
        Box::pin(async move {
            let files = blocking("list", root.to_string(), move || {
                Local.files(&at, glob.as_deref(), Listing::All)
            })
            .await?;
            files
                .iter()
                .map(|file| sandbox_path("list", file))
                .collect()
        })
    }

    fn canonical<'a>(&'a self, path: &'a SandboxPath) -> BoxFuture<'a, Result<SandboxPath>> {
        let at = host(path);
        Box::pin(async move {
            let canonical = blocking("canonical", path.to_string(), move || {
                Ok(Local.canonical(&at))
            })
            .await?;
            sandbox_path("canonical", &canonical)
        })
    }

    fn read_files<'a>(
        &'a self,
        paths: &'a [SandboxPath],
        limit: u64,
    ) -> BoxFuture<'a, Vec<Option<Vec<u8>>>> {
        let at: Vec<PathBuf> = paths.iter().map(host).collect();
        let count = paths.len();
        Box::pin(async move {
            blocking("read_files", String::new(), move || {
                Ok(Local.read_files(&at, limit))
            })
            .await
            .unwrap_or_else(|_| vec![None; count])
        })
    }

    fn exec(&self, request: ExecRequest) -> BoxFuture<'_, Result<Execution>> {
        let env = self.env.clone();
        Box::pin(async move {
            let (program, args) = match request.command {
                Command::Shell(line) => ("sh".to_owned(), vec!["-c".to_owned(), line]),
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
            // `env -i` starts the command from these variables alone, never the host's.
            let mut vars: Vec<(String, String)> = env.as_ref().clone();
            vars.push(("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()));
            for (name, value) in request.env {
                vars.retain(|(other, _)| *other != name);
                if let Some(value) = value {
                    vars.push((name, value));
                }
            }
            let prefix = ["/usr/bin/env".to_owned(), "-i".to_owned()]
                .into_iter()
                .chain(
                    vars.into_iter()
                        .map(|(name, value)| format!("{name}={value}")),
                );
            let launcher = Launcher::default().wrapped(prefix);
            let cwd = PathBuf::from(request.cwd.unwrap_or_else(|| "/".to_owned()));
            let how = Spawn {
                stdin: request.stdin,
                merge_stderr: request.merge_stderr,
            };
            let timeout = request.timeout;
            let shown = program.clone();
            let mut process = blocking("exec", shown, move || {
                let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
                Local.spawn(&launcher, Some(&cwd), program.as_ref(), &args, how)
            })
            .await?;

            let (send_output, output) = mpsc::unbounded();
            let (send_exit, exit) = oneshot::channel();
            let killed = Arc::new(AtomicBool::new(false));
            let group = process.group();
            let was_killed = killed.clone();
            tokio::task::spawn_blocking(move || {
                let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
                let ended = process.run(deadline, |line| {
                    let chunk = match line {
                        OutputLine::Out(text) => Chunk::Out(text),
                        OutputLine::Err(text) => Chunk::Err(text),
                    };
                    // Unread output is dropped; the command keeps running.
                    send_output.unbounded_send(chunk).ok();
                    true
                });
                drop(send_output);
                let exit = ended
                    .map(|ended| match ended {
                        _ if was_killed.load(Ordering::SeqCst) => Exit::Killed,
                        Ended::Exited(status) => exit_of(status),
                        Ended::TimedOut => Exit::TimedOut,
                        Ended::Stopped => Exit::Killed,
                    })
                    .map_err(|e| io_error("exec", String::new(), &e));
                send_exit.send(exit).ok();
            });
            let exit = async move {
                exit.await
                    .unwrap_or_else(|_| Err(SandboxError::Died { operation: "exec" }.into()))
            };
            let kill = move || {
                killed.store(true, Ordering::SeqCst);
                // SIGTERM now, SIGKILL after two seconds if it still runs.
                drop(KillOnDrop(group));
            };
            Ok(Execution::new(output, exit, kill))
        })
    }
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
