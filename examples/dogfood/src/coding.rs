//! Coding tools (`read`, `write`, `edit`, `search`, `list`, `shell`) as keelrs sandboxed tools,
//! until keelrs has its own built-ins (#42).
//!
//! `shell` runs through the turn's [`Sandbox`]: it starts in the work dir, from the sandbox's
//! env allowlist. The file tools are a stand-in: rig-tools' own, confined to the sandbox's work
//! dir but reading and writing this machine's disk directly, which is only right for
//! `sandbox::local()`.
//!
//! keelrs runs a reply's calls side by side and does not read Rig's footprints, so two calls
//! that change one file can each write over the other; the implementer's instructions ask it
//! never to send such calls in one reply.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use keelrs::sandbox::{ExecRequest, Exit, Sandbox};
use keelrs::{Tool, ToolCall};
use rig_core::tool::{Tool as RigTool, ToolContext};
use rig_tools::{Edit, List, Read, Search, Shell, ShellArgs, Write};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

/// Longest a `shell` command runs unless the model asks for less; it then reads that it timed out.
const SHELL_TIMEOUT: Duration = Duration::from_mins(5);
/// How much of a command's output the model reads: its last 2000 lines, at most 50 KB.
const SHOWN_LINES: usize = 2000;
const SHOWN_BYTES: usize = 50 * 1024;

/// `read`, `search`, `list` and `shell`: looking around, and `git`/`gh` through `shell`.
pub static LOOKING: LazyLock<Vec<Tool>> = LazyLock::new(|| looking(Read::default()));

/// [`LOOKING`], plus `write` and `edit`, which share what its `read` has seen.
pub static CODING: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    let read = Read::default();
    let seen = read.seen.clone();
    let mut tools = looking(read);
    tools.push(rig(
        Write {
            seen: seen.clone(),
            ..Write::default()
        },
        |tool, dir| {
            tool.base = Some(dir);
            tool.confine = true;
        },
    ));
    tools.push(rig(
        Edit {
            seen,
            ..Edit::default()
        },
        |tool, dir| {
            tool.base = Some(dir);
            tool.confine = true;
        },
    ));
    tools
});

fn looking(read: Read) -> Vec<Tool> {
    vec![
        rig(read, |tool, dir| {
            tool.base = Some(dir);
            tool.confine = true;
        }),
        rig(Search::default(), |tool, dir| {
            tool.base = Some(dir);
            tool.confine = true;
        }),
        rig(List::default(), |tool, dir| {
            tool.base = Some(dir);
            tool.confine = true;
        }),
        shell(),
    ]
}

/// A Rig file tool as a keelrs sandboxed tool, pointed at the sandbox's work dir by `confine`
/// before each call; each call is logged to stderr.
fn rig<T>(tool: T, confine: fn(&mut T, PathBuf)) -> Tool
where
    T: RigTool + Clone + Send + Sync + 'static,
    T::Args: DeserializeOwned + JsonSchema + std::fmt::Debug + Send + 'static,
    T::Output: serde::Serialize + 'static,
    T::Error: Send + Sync,
{
    let description = tool.description();
    Tool::sandbox(
        T::NAME,
        description,
        move |call: ToolCall<T::Args>, sandbox: Sandbox| async move {
            log(T::NAME, &call.data);
            let mut tool = tool;
            confine(&mut tool, PathBuf::from(sandbox.dir().as_str()));
            tool.call(&mut ToolContext::new(), call.data).await
        },
    )
}

/// `shell` through the sandbox: its shell's `-c` in the work dir, stderr merged into stdout, and the
/// last [`SHOWN_LINES`] lines or [`SHOWN_BYTES`] of the output shown, with how it ended.
fn shell() -> Tool {
    Tool::sandbox(
        "shell",
        Shell::default().description(),
        async |call: ToolCall<ShellArgs>, sandbox: Sandbox| {
            log("shell", &call.data);
            let ShellArgs {
                command,
                timeout_secs,
                cwd,
            } = call.data;
            let timeout = timeout_secs.map_or(SHELL_TIMEOUT, |secs| {
                Duration::from_secs(secs).min(SHELL_TIMEOUT)
            });
            let mut request = ExecRequest::shell(command).merge_stderr().timeout(timeout);
            if let Some(cwd) = cwd {
                request = request.cwd(cwd);
            }
            let ran = sandbox.exec(request).await?.run().await?;
            let ended = match ran.exit {
                Exit::Code(code) => format!("exit code {code}"),
                Exit::Signaled(signal) => format!("ended by signal {signal}"),
                Exit::TimedOut => format!("timed out after {timeout:?} and was stopped"),
                Exit::Killed => "killed".to_owned(),
            };
            Ok::<_, keelrs::Error>(format!("{}\n[{ended}]", tail(&ran.stdout)))
        },
    )
}

/// The last [`SHOWN_LINES`] lines of `output`, at most [`SHOWN_BYTES`], saying what was cut.
fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let (mut start, mut bytes) = (lines.len(), 0);
    while start > 0
        && lines.len() - start < SHOWN_LINES
        && bytes + lines[start - 1].len() < SHOWN_BYTES
    {
        start -= 1;
        bytes += lines[start].len() + 1;
    }
    let shown = lines[start..].join("\n");
    if start == 0 {
        shown
    } else {
        format!("[{start} earlier lines cut]\n{shown}")
    }
}

fn log(name: &str, data: &impl std::fmt::Debug) {
    eprintln!("  {name} {}", rig_tools::shorten(&format!("{data:?}"), 160));
}
