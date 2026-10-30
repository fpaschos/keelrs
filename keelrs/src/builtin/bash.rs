//! `bash`: a command line in the sandbox's shell.

use std::time::Duration;

use rig_core::tool::ToolExecutionError;
use rig_tools::{MAX_BYTES, MAX_LINES, OutputLimit};
use schemars::JsonSchema;
use serde::Deserialize;

use super::Out;
use crate::sandbox::{Chunk, ExecRequest, Exit, Sandbox, SandboxPath};
use crate::{Tool, ToolCall};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BashArgs {
    /// The command line to run.
    pub(crate) command: String,
    /// Seconds it may run before it is stopped; no limit when not given.
    pub(crate) timeout: Option<u64>,
}

pub(super) fn tool(shell: Option<&SandboxPath>) -> Tool {
    Tool::sandbox(
        "bash",
        description(shell),
        |call: ToolCall<BashArgs>, sb: Sandbox| async move { bash(&sb, call.data).await },
    )
}

/// What the model reads of `bash`, naming `shell` when it is not bash.
pub(super) fn description(shell: Option<&SandboxPath>) -> String {
    let mut text = format!(
        "Run a command line in the working directory, for builds, tests, git and other \
         programs. Returns its output, stderr merged into stdout, at most the last {MAX_LINES} \
         lines or {} KB, then how it ended. It runs until it ends, or until it has printed \
         {} MB, unless `timeout` (seconds) is given. Use `read`, `grep`, `glob`, `edit` and `write` for files, not `cat`, \
         `grep`, `find`, `sed` or `echo`.",
        MAX_BYTES / 1024,
        MAX_PRINTED / (1024 * 1024)
    );
    if let Some(shell) = shell.filter(|shell| !shell.as_str().ends_with("/bash")) {
        text.push_str(&format!(
            " The shell is `{shell}`, not bash: write POSIX sh, without bashisms such as \
             `[[ ]]`, arrays, `source` or `{{a,b}}`."
        ));
    }
    text
}

/// Runs `args.command` to its end, or until it has printed
/// [`MAX_PRINTED`]: its output's tail, then how it ended.
pub(crate) async fn bash(sb: &Sandbox, args: BashArgs) -> Out {
    let mut request = ExecRequest::shell(args.command).merge_stderr();
    let timeout = args.timeout.map(Duration::from_secs);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    let failed = |e: crate::Error| ToolExecutionError::other(e.to_string());
    let mut execution = sb.exec(request).await.map_err(failed)?;
    let mut tail = Tail::default();
    let mut stopped = false;
    while let Some(Chunk::Out(text) | Chunk::Err(text)) = execution.next().await {
        tail.push(&text);
        if tail.printed > MAX_PRINTED {
            execution.kill();
            stopped = true;
            break;
        }
    }
    let exit = execution.wait().await.map_err(failed)?;
    let ended = match exit {
        _ if stopped => format!("stopped after printing {} MB", MAX_PRINTED / (1024 * 1024)),
        Exit::Code(code) => format!("exit code {code}"),
        Exit::Signaled(signal) => format!("ended by signal {signal}"),
        Exit::TimedOut => format!(
            "timed out after {}s and was stopped",
            args.timeout.unwrap_or_default()
        ),
        Exit::Killed => "killed".to_owned(),
    };
    let mut out = String::new();
    match OutputLimit::default().tail(&tail.kept) {
        ("", _) if !tail.kept.is_empty() => {
            let mut start = tail.kept.len().saturating_sub(MAX_BYTES);
            while !tail.kept.is_char_boundary(start) {
                start += 1;
            }
            out.push_str(&format!(
                "[showing the last {} KB of a longer line]\n",
                MAX_BYTES / 1024
            ));
            out.push_str(&tail.kept[start..]);
        }
        (kept, cut) => {
            let cut = cut + tail.cut;
            if cut > 0 {
                out.push_str(&format!("[{cut} earlier lines cut]\n"));
            }
            out.push_str(kept);
        }
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("[{ended}]"));
    Ok(out)
}

/// Most output one `bash` call lets a command print before stopping it, as
/// rig-tools' shell does.
const MAX_PRINTED: usize = 10 * 1024 * 1024;

/// The end of a command's output, enough for [`OutputLimit::tail`]: at
/// least the last [`MAX_BYTES`], and at most twice that.
#[derive(Default)]
struct Tail {
    kept: String,
    /// Lines ended in what was dropped from the front of `kept`.
    cut: usize,
    /// Bytes printed in all.
    printed: usize,
}

impl Tail {
    fn push(&mut self, text: &str) {
        self.printed += text.len();
        self.kept.push_str(text);
        if self.kept.len() > 2 * MAX_BYTES {
            let mut drop = self.kept.len() - MAX_BYTES;
            while !self.kept.is_char_boundary(drop) {
                drop += 1;
            }
            self.cut += self.kept[..drop].matches('\n').count();
            self.kept.drain(..drop);
        }
    }
}
