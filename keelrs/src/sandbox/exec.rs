//! Running a command in a sandbox: what to run ([`ExecRequest`]), the
//! running command ([`Execution`]) and how it ended ([`Ran`], [`Exit`]).

use std::fmt;
use std::future::Future;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt as _, Stream, StreamExt as _};
use serde::{Deserialize, Serialize};

use crate::Result;
#[cfg(doc)]
use crate::sandbox::Sandbox;

/// How much of each stream [`Execution::run`] keeps: the last 64 MiB.
const KEPT_OUTPUT: usize = 64 * 1024 * 1024;

/// What a command runs: a shell line or a program with its arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// A line for the sandbox's [shell](Sandbox::shell), run with `-c`.
    Shell(String),
    /// A program, then its arguments.
    Argv(Vec<String>),
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shell(line) => f.write_str(line),
            Self::Argv(argv) => f.write_str(&argv.join(" ")),
        }
    }
}

/// A command to run in a sandbox, built with [`shell`](Self::shell) or
/// [`argv`](Self::argv).
#[derive(Clone, Debug, Serialize, Deserialize)]
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

impl<S: Into<String>, const N: usize> From<[S; N]> for ExecRequest {
    fn from(argv: [S; N]) -> Self {
        Self::argv(argv)
    }
}

impl<S: Into<String>> From<Vec<S>> for ExecRequest {
    fn from(argv: Vec<S>) -> Self {
        Self::argv(argv)
    }
}

/// A piece of a running command's output, as it comes: usually whole lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// From stdout.
    Out(String),
    /// From stderr.
    Err(String),
}

/// How a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit code {code}"),
            Self::Signaled(signal) => write!(f, "signal {signal}"),
            Self::TimedOut => f.write_str("its timeout"),
            Self::Killed => f.write_str("a kill"),
        }
    }
}

/// What a command printed and how it ended, from [`Execution::run`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        keep_last(&mut stdout, KEPT_OUTPUT);
        keep_last(&mut stderr, KEPT_OUTPUT);
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

/// Appends `text` to `kept`, dropping from the front what is past
/// [`KEPT_OUTPUT`] once it is an eighth over, so the drop that moves the
/// rest comes once per 8 MiB, not once per line.
fn keep_tail(kept: &mut String, text: &str) {
    kept.push_str(text);
    if kept.len() > KEPT_OUTPUT + KEPT_OUTPUT / 8 {
        keep_last(kept, KEPT_OUTPUT);
    }
}

/// Drops from the front of `kept` what is past `max` bytes, at a character
/// boundary; the text dropped.
pub(crate) fn keep_last(kept: &mut String, max: usize) -> std::string::Drain<'_> {
    let cut = kept.ceil_char_boundary(kept.len().saturating_sub(max));
    kept.drain(..cut)
}
