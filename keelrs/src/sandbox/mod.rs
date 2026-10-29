//! Where tools read files and run commands.
//!
//! A [`Sandbox`] is a cheap, cloneable handle over a [`SandboxBackend`]:
//! this machine ([`local()`]), or later a container, microVM or remote
//! host. The application opens it, shares it by cloning, and declares it
//! for a turn with
//! [`AgentContext::use_sandbox`](crate::AgentContext::use_sandbox); keelrs
//! never creates or removes one on its own, and holds one only for the
//! dispatch that declared it.
//!
//! What keelrs guarantees, and what it does not:
//!
//! - A sandboxed tool ([`Tool::sandbox`](crate::Tool::sandbox)) receives
//!   the sandbox its turn declared, and no other.
//! - File operations stay inside the sandbox's [work dir](Sandbox::work_dir):
//!   a path that leads out of it, through `..` or a link, is refused.
//! - Commands start in the work dir but are not caged: a command can read
//!   and write whatever the backend lets it, such as the whole machine for
//!   `local()`.
//! - Host tools ([`Tool::host`](crate::Tool::host)) are trusted code that
//!   runs outside any sandbox; keelrs cannot stop one from touching the
//!   host (ADR 0005).
//!
//! To keep a crate's own tools from reaching around the sandbox, deny the
//! host's file and process APIs with clippy, as keelrs does in its own
//! `clippy.toml`:
//!
//! ```toml
//! disallowed-methods = [
//!     { path = "std::fs::read", reason = "go through the Sandbox" },
//!     { path = "std::process::Command::new", reason = "go through Sandbox::exec" },
//!     # …and the rest of `std::fs`, `tokio::fs` and `tokio::process`
//! ]
//! disallowed-types = [{ path = "std::fs::File", reason = "go through the Sandbox" }]
//! ```
//!
//! and allow them only where a backend is implemented.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt as _, Stream, StreamExt as _};
use tokio_util::sync::CancellationToken;

use crate::Result;

mod local;
mod path;

pub use local::{LocalBuilder, local};
pub use path::SandboxPath;

/// How much of each stream [`Execution::run`] keeps: the last 64 MiB.
const KEPT_OUTPUT: usize = 64 * 1024 * 1024;

/// What a path is, its links followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    /// A regular file.
    File {
        /// Its size in bytes.
        size: u64,
        /// Whether it may not be written.
        read_only: bool,
        /// When its content last changed, when the backend can tell.
        modified: Option<SystemTime>,
    },
    /// A directory.
    Dir,
    /// Something else, such as a FIFO or a device.
    Other,
    /// Nothing is there.
    Missing,
}

/// What a read read: at most the asked number of bytes, and the file's whole size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    /// At most the asked number of bytes, from the file's start.
    pub bytes: Vec<u8>,
    /// The file's whole size, more than `bytes` holds when it was cut.
    pub size: u64,
}

/// The SHA-256 of a file's content, as lowercase hex: what
/// [`Sandbox::write_if`] compares against.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash(String);

impl ContentHash {
    /// The hash of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(local::content_hash(bytes))
    }

    /// The hash as lowercase hex.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which operations a backend supports; the others fail with
/// [`SandboxError::Unsupported`] before reaching it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    /// `write`, `write_if`, `remove` and `rename`.
    pub write: bool,
    /// `exec`.
    pub exec: bool,
}

impl Capabilities {
    /// Every operation.
    pub const ALL: Self = Self {
        write: true,
        exec: true,
    };
}

/// What a sandbox could not do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// The sandbox went away during the operation, or was closed.
    #[error("the sandbox died during `{operation}`")]
    Died {
        /// The operation, such as `read`.
        operation: &'static str,
    },
    /// The backend does not support the operation.
    #[error("this sandbox does not support `{operation}`")]
    Unsupported {
        /// The operation, such as `exec`.
        operation: &'static str,
    },
    /// A file operation named a path outside the work dir.
    #[error("`{path}` is outside the work dir `{work_dir}`")]
    OutsideWorkDir {
        /// The path, canonical.
        path: SandboxPath,
        /// The work dir, canonical.
        work_dir: SandboxPath,
    },
    /// The operation failed inside the sandbox.
    #[error("`{operation}` of `{path}` failed: {message}")]
    Io {
        /// The operation, such as `write`.
        operation: &'static str,
        /// The path, or the program for `exec`.
        path: String,
        /// What kind of failure it was.
        kind: std::io::ErrorKind,
        /// What went wrong, as plain text.
        message: String,
    },
}

/// A sandbox's identity, unique within this process: what tells two
/// sandboxes apart, such as in a tool call's footprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SandboxId(u64);

impl SandboxId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Display for SandboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sandbox-{}", self.0)
    }
}

/// What a sandbox does: the operations a backend implements, on absolute,
/// canonical [`SandboxPath`]s that [`Sandbox`] has already confined to its
/// work dir.
///
/// Implement it for a new kind of sandbox with
/// [`#[keelrs::async_trait]`](crate::async_trait) and wrap it with
/// [`Sandbox::new`]:
///
/// ```
/// use keelrs::Result;
/// use keelrs::sandbox::{
///     Contents, ExecRequest, Execution, SandboxBackend, SandboxError, SandboxPath, Stat,
/// };
///
/// /// A sandbox with nothing in it.
/// struct Empty;
///
/// #[keelrs::async_trait]
/// impl SandboxBackend for Empty {
///     async fn stat(&self, _path: &SandboxPath) -> Result<Stat> {
///         Ok(Stat::Missing)
///     }
///
///     async fn read(&self, _path: &SandboxPath, _limit: u64) -> Result<Contents> {
///         Err(SandboxError::Unsupported { operation: "read" }.into())
///     }
///     // …write, remove, rename, list, canonical and exec alike
/// #   async fn write(&self, _: &SandboxPath, _: Vec<u8>) -> Result<()> { Ok(()) }
/// #   async fn remove(&self, _: &SandboxPath, _: bool) -> Result<()> { Ok(()) }
/// #   async fn rename(&self, _: &SandboxPath, _: &SandboxPath) -> Result<()> { Ok(()) }
/// #   async fn list(&self, _: &SandboxPath, _: Option<&str>) -> Result<Vec<SandboxPath>> { Ok(vec![]) }
/// #   async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> { Ok(path.clone()) }
/// #   async fn exec(&self, _: ExecRequest) -> Result<Execution> {
/// #       Err(SandboxError::Unsupported { operation: "exec" }.into())
/// #   }
/// }
///
/// let sb = keelrs::sandbox::Sandbox::new(Empty, SandboxPath::new("/").unwrap());
/// ```
#[async_trait::async_trait]
pub trait SandboxBackend: Send + Sync + 'static {
    /// What `path` is; nothing there is [`Stat::Missing`], not an error.
    async fn stat(&self, path: &SandboxPath) -> Result<Stat>;

    /// Up to `limit` bytes of the regular file `path`, and its whole size.
    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents>;

    /// Replaces the file `path` with `bytes` in one step, creating its directory.
    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()>;

    /// Removes the file `path`; a directory only when empty, or with `recursive`.
    async fn remove(&self, path: &SandboxPath, recursive: bool) -> Result<()>;

    /// Moves the file `from` to `to`, creating `to`'s directory and replacing a file there.
    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()>;

    /// The regular files under `root`, only those matching `glob` (such as
    /// `*.rs`) when it is given.
    async fn list(&self, root: &SandboxPath, glob: Option<&str>) -> Result<Vec<SandboxPath>>;

    /// `path` with its links followed and `..` folded, so two spellings of
    /// one file come out alike; a missing tail is kept as it is.
    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath>;

    /// Starts `request`, whose `cwd` is absolute.
    async fn exec(&self, request: ExecRequest) -> Result<Execution>;

    /// [`write`](Self::write)s `bytes` to `path` only while it holds the
    /// content hashed as `held`: `None` when it did, and otherwise what it
    /// holds instead. By default it reads, compares, then writes, which is
    /// not atomic.
    async fn write_if(
        &self,
        path: &SandboxPath,
        bytes: Vec<u8>,
        held: ContentHash,
    ) -> Result<Option<Contents>> {
        let now = self.read(path, u64::MAX).await?;
        if ContentHash::of(&now.bytes) != held {
            return Ok(Some(now));
        }
        self.write(path, bytes).await?;
        Ok(None)
    }

    /// The whole content of each of `paths` that is a regular file of at
    /// most `limit` bytes, in order; `None` for any other. By default one
    /// [`read`](Self::read) each.
    async fn read_files(&self, paths: &[SandboxPath], limit: u64) -> Vec<Option<Vec<u8>>> {
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let file = match self.read(path, limit.saturating_add(1)).await {
                Ok(contents) if contents.size <= limit => Some(contents.bytes),
                _ => None,
            };
            files.push(file);
        }
        files
    }

    /// Which operations it supports.
    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    /// The shell that runs [`Command::Shell`] lines, by its path in the
    /// sandbox; `None` when the backend does not say.
    fn shell(&self) -> Option<&SandboxPath> {
        None
    }

    /// Removes the sandbox and what runs in it; nothing by default.
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// Where sandboxed tools read files and run commands: a cheap, cloneable
/// handle over a [`SandboxBackend`], narrowed to a [work dir](Self::work_dir).
///
/// Relative paths start at the work dir, and file operations that lead out
/// of it are refused with [`SandboxError::OutsideWorkDir`]. Commands start
/// there but are not caged. Every clone shares one backend: once
/// [`close`](Self::close)d, operations on any clone fail with
/// [`SandboxError::Died`]. Dropping a handle removes nothing.
#[derive(Clone)]
pub struct Sandbox {
    id: SandboxId,
    work_dir: SandboxPath,
    backend: Arc<dyn SandboxBackend>,
    closed: CancellationToken,
}

impl Sandbox {
    /// A sandbox over `backend`, whose work dir is `root`: the backend's
    /// root or mount point.
    pub fn new(backend: impl SandboxBackend, root: SandboxPath) -> Self {
        Self {
            id: SandboxId::next(),
            work_dir: root,
            backend: Arc::new(backend),
            closed: CancellationToken::new(),
        }
    }

    /// Its identity, shared by every clone and narrowed work dir.
    #[must_use]
    pub fn id(&self) -> SandboxId {
        self.id
    }

    /// The directory relative paths start at and file operations stay in.
    #[must_use]
    pub fn dir(&self) -> &SandboxPath {
        &self.work_dir
    }

    /// A clone whose work dir is `dir`, relative to this one's.
    #[must_use]
    pub fn work_dir(&self, dir: impl AsRef<str>) -> Self {
        Self {
            work_dir: self.work_dir.join(dir.as_ref()),
            ..self.clone()
        }
    }

    /// Which operations its backend supports.
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    /// The shell that runs [`Command::Shell`] lines, as its backend names
    /// it; `None` when the backend does not say.
    #[must_use]
    pub fn shell(&self) -> Option<&SandboxPath> {
        self.backend.shell()
    }

    /// What `path` is; nothing there is [`Stat::Missing`].
    pub async fn stat(&self, path: impl AsRef<str>) -> Result<Stat> {
        let path = self.place(path.as_ref()).await?;
        self.race("stat", self.backend.stat(&path)).await
    }

    /// Up to `limit` bytes of the regular file `path`, and its whole size.
    pub async fn read(&self, path: impl AsRef<str>, limit: u64) -> Result<Contents> {
        let path = self.place(path.as_ref()).await?;
        self.race("read", self.backend.read(&path, limit)).await
    }

    /// The whole content of each of `paths` that is a regular file of at
    /// most `limit` bytes, in order; `None` for any other, and for one
    /// outside the work dir.
    pub async fn read_files<P: AsRef<str>>(
        &self,
        paths: &[P],
        limit: u64,
    ) -> Result<Vec<Option<Vec<u8>>>> {
        let mut placed = Vec::with_capacity(paths.len());
        for path in paths {
            placed.push(self.place(path.as_ref()).await.ok());
        }
        let inside: Vec<SandboxPath> = placed.iter().flatten().cloned().collect();
        let mut read = self
            .race(
                "read_files",
                self.backend.read_files(&inside, limit).map(Ok),
            )
            .await?
            .into_iter();
        Ok(placed
            .iter()
            .map(|path| path.as_ref().and_then(|_| read.next().flatten()))
            .collect())
    }

    /// Replaces the file `path` with `bytes` in one step, creating its directory.
    pub async fn write(&self, path: impl AsRef<str>, bytes: impl Into<Vec<u8>>) -> Result<()> {
        supports("write", self.capabilities().write)?;
        let path = self.place(path.as_ref()).await?;
        self.race("write", self.backend.write(&path, bytes.into()))
            .await
    }

    /// Writes `bytes` to the file `path` only while it holds the content
    /// hashed as `held`: `None` when it did, and otherwise what it holds
    /// instead, with nothing written.
    pub async fn write_if(
        &self,
        path: impl AsRef<str>,
        bytes: impl Into<Vec<u8>>,
        held: ContentHash,
    ) -> Result<Option<Contents>> {
        supports("write_if", self.capabilities().write)?;
        let path = self.place(path.as_ref()).await?;
        self.race("write_if", self.backend.write_if(&path, bytes.into(), held))
            .await
    }

    /// Removes the file `path`; a directory only when empty, or with `recursive`.
    pub async fn remove(&self, path: impl AsRef<str>, recursive: bool) -> Result<()> {
        supports("remove", self.capabilities().write)?;
        let path = self.place(path.as_ref()).await?;
        self.race("remove", self.backend.remove(&path, recursive))
            .await
    }

    /// Moves the file `from` to `to`, creating `to`'s directory and replacing a file there.
    pub async fn rename(&self, from: impl AsRef<str>, to: impl AsRef<str>) -> Result<()> {
        supports("rename", self.capabilities().write)?;
        let from = self.place(from.as_ref()).await?;
        let to = self.place(to.as_ref()).await?;
        self.race("rename", self.backend.rename(&from, &to)).await
    }

    /// The regular files under `root`, only those matching `glob` (such as
    /// `*.rs`) when it is given.
    pub async fn list(
        &self,
        root: impl AsRef<str>,
        glob: Option<&str>,
    ) -> Result<Vec<SandboxPath>> {
        let root = self.place(root.as_ref()).await?;
        self.race("list", self.backend.list(&root, glob)).await
    }

    /// `path` from the work dir, its links followed and `..` folded; refused
    /// when that leads outside the work dir.
    pub async fn canonical(&self, path: impl AsRef<str>) -> Result<SandboxPath> {
        self.place(path.as_ref()).await
    }

    /// Starts `request` in its `cwd` from the work dir, or the work dir
    /// itself. The command is not caged to the work dir.
    pub async fn exec(&self, mut request: ExecRequest) -> Result<Execution> {
        supports("exec", self.capabilities().exec)?;
        let cwd = self.work_dir.join(request.cwd.as_deref().unwrap_or("."));
        request.cwd = Some(cwd.to_string());
        self.race("exec", self.backend.exec(request)).await
    }

    /// Removes the sandbox through its backend; afterwards every clone's
    /// operations fail with [`SandboxError::Died`].
    pub async fn close(self) -> Result<()> {
        self.closed.cancel();
        self.backend.close().await
    }

    /// `path` from the work dir, canonical, or the error when it leads outside.
    async fn place(&self, path: &str) -> Result<SandboxPath> {
        let joined = self.work_dir.join(path);
        let path = self
            .race("canonical", self.backend.canonical(&joined))
            .await?;
        let work_dir = self
            .race("canonical", self.backend.canonical(&self.work_dir))
            .await?;
        if path.starts_with(&work_dir) {
            Ok(path)
        } else {
            Err(SandboxError::OutsideWorkDir { path, work_dir }.into())
        }
    }

    /// `operation`, or [`SandboxError::Died`] once the sandbox is closed.
    async fn race<T>(
        &self,
        operation: &'static str,
        work: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        self.closed
            .run_until_cancelled(work)
            .await
            .unwrap_or_else(|| Err(SandboxError::Died { operation }.into()))
    }
}

/// `Ok` when `supported`, else [`SandboxError::Unsupported`] for `operation`.
fn supports(operation: &'static str, supported: bool) -> Result<()> {
    if supported {
        Ok(())
    } else {
        Err(SandboxError::Unsupported { operation }.into())
    }
}

impl fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sandbox")
            .field("id", &self.id)
            .field("work_dir", &self.work_dir)
            .finish_non_exhaustive()
    }
}

/// What a command runs: a shell line or a program with its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// A line for the sandbox's [shell](Sandbox::shell), run with `-c`.
    Shell(String),
    /// A program, then its arguments.
    Argv(Vec<String>),
}

/// A command to run in a sandbox, built with [`shell`](Self::shell) or
/// [`argv`](Self::argv).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ExecRequest {
    /// What runs.
    pub command: Command,
    /// Where it starts, from the work dir; absolute once it reaches the backend.
    pub cwd: Option<String>,
    /// Variables set (`Some`) or removed (`None`), in order, over the backend's own.
    pub env: Vec<(String, Option<String>)>,
    /// What it reads on stdin; `None` is an empty stdin.
    pub stdin: Option<Vec<u8>>,
    /// How long it may run before it is stopped; `None` has no deadline.
    pub timeout: Option<Duration>,
    /// Whether stderr goes into stdout, so the two keep their order.
    pub merge_stderr: bool,
}

impl ExecRequest {
    /// Runs `line` with the sandbox's [shell](Sandbox::shell) and `-c`.
    #[must_use]
    pub fn shell(line: impl Into<String>) -> Self {
        Self::of(Command::Shell(line.into()))
    }

    /// Runs the program `argv[0]` with the rest as its arguments.
    #[must_use]
    pub fn argv<I>(argv: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        Self::of(Command::Argv(argv.into_iter().map(Into::into).collect()))
    }

    fn of(command: Command) -> Self {
        Self {
            command,
            cwd: None,
            env: Vec::new(),
            stdin: None,
            timeout: None,
            merge_stderr: false,
        }
    }

    /// Starts in `dir`, from the work dir.
    #[must_use]
    pub fn cwd(mut self, dir: impl Into<String>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Sets `name` to `value`.
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), Some(value.into())));
        self
    }

    /// Removes `name`.
    #[must_use]
    pub fn env_remove(mut self, name: impl Into<String>) -> Self {
        self.env.push((name.into(), None));
        self
    }

    /// Feeds `bytes` on stdin, then closes it.
    #[must_use]
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Stops it after `timeout`; it then ends as [`Exit::TimedOut`].
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sends stderr into stdout, so the two keep their order.
    #[must_use]
    pub fn merge_stderr(mut self) -> Self {
        self.merge_stderr = true;
        self
    }
}

/// A piece of a running command's output, as it comes: usually a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// From stdout.
    Out(String),
    /// From stderr.
    Err(String),
}

/// How a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this code.
    Code(i32),
    /// A signal it did not ask for ended it.
    Signaled(i32),
    /// It ran past its timeout and was stopped.
    TimedOut,
    /// It was [killed](Execution::kill), or its execution dropped.
    Killed,
}

/// What a command printed and how it ended, from [`Execution::run`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    /// The last 64 MiB of stdout (all output, with `merge_stderr`).
    pub stdout: String,
    /// The last 64 MiB of stderr.
    pub stderr: String,
    /// How it ended.
    pub exit: Exit,
}

/// A running command: its output as it comes, then how it ended. Dropping
/// it before the end kills the command.
pub struct Execution {
    output: BoxStream<'static, Chunk>,
    exit: BoxFuture<'static, Result<Exit>>,
    kill: Option<Box<dyn FnOnce() + Send>>,
}

impl Execution {
    /// An execution a backend builds from its output stream, which ends
    /// when the command's output does, a future of how it ended, and how to
    /// stop it, which must make it end as [`Exit::Killed`].
    pub fn new(
        output: impl Stream<Item = Chunk> + Send + 'static,
        exit: impl Future<Output = Result<Exit>> + Send + 'static,
        kill: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            output: output.boxed(),
            exit: exit.boxed(),
            kill: Some(Box::new(kill)),
        }
    }

    /// The next piece of output; `None` once the output ended.
    pub async fn next(&mut self) -> Option<Chunk> {
        self.output.next().await
    }

    /// Stops the command; it then ends as [`Exit::Killed`].
    pub fn kill(&mut self) {
        if let Some(kill) = self.kill.take() {
            kill();
        }
    }

    /// Waits for it to end; output left unread is discarded.
    pub async fn wait(mut self) -> Result<Exit> {
        self.output = futures::stream::empty().boxed();
        let exit = (&mut self.exit).await;
        self.kill = None;
        exit
    }

    /// Runs it to its end, keeping the last 64 MiB of each stream.
    pub async fn run(mut self) -> Result<Ran> {
        let (mut stdout, mut stderr) = (String::new(), String::new());
        while let Some(chunk) = self.next().await {
            match chunk {
                Chunk::Out(text) => keep_tail(&mut stdout, &text),
                Chunk::Err(text) => keep_tail(&mut stderr, &text),
            }
        }
        let exit = self.wait().await?;
        Ok(Ran {
            stdout,
            stderr,
            exit,
        })
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.kill();
    }
}

impl fmt::Debug for Execution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Execution").finish_non_exhaustive()
    }
}

/// Appends `text` to `kept`, then drops from the front what is past
/// [`KEPT_OUTPUT`], at a character boundary.
fn keep_tail(kept: &mut String, text: &str) {
    kept.push_str(text);
    if kept.len() > KEPT_OUTPUT {
        let mut cut = kept.len() - KEPT_OUTPUT;
        while !kept.is_char_boundary(cut) {
            cut += 1;
        }
        kept.drain(..cut);
    }
}
