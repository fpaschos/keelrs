//! `bash`: a command line in the sandbox's shell.

use std::time::Duration;

use rig_core::tool::ToolExecutionError;
use rig_tools::{MAX_BYTES, MAX_LINES, OutputLimit};
use schemars::JsonSchema;
use serde::Deserialize;

use super::Out;
use crate::sandbox::{ExecRequest, Exit, Sandbox, SandboxPath};
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
         lines or {} KB, then how it ended. It runs until it ends unless `timeout` (seconds) \
         is given. Use `read`, `grep`, `glob`, `edit` and `write` for files, not `cat`, \
         `grep`, `find`, `sed` or `echo`.",
        MAX_BYTES / 1024
    );
    if let Some(shell) = shell.filter(|shell| !shell.as_str().ends_with("/bash")) {
        text.push_str(&format!(
            " The shell is `{shell}`, not bash: write POSIX sh, without bashisms such as \
             `[[ ]]`, arrays, `source` or `{{a,b}}`."
        ));
    }
    text
}

/// Runs `args.command` to its end: its output's tail, then how it ended.
pub(crate) async fn bash(sb: &Sandbox, args: BashArgs) -> Out {
    let mut request = ExecRequest::shell(args.command).merge_stderr();
    let timeout = args.timeout.map(Duration::from_secs);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    let failed = |e: crate::Error| ToolExecutionError::other(e.to_string());
    let ran = sb
        .exec(request)
        .await
        .map_err(failed)?
        .run()
        .await
        .map_err(failed)?;
    let ended = match ran.exit {
        Exit::Code(code) => format!("exit code {code}"),
        Exit::Signaled(signal) => format!("ended by signal {signal}"),
        Exit::TimedOut => format!(
            "timed out after {}s and was stopped",
            args.timeout.unwrap_or_default()
        ),
        Exit::Killed => "killed".to_owned(),
    };
    let (kept, cut) = OutputLimit::default().tail(&ran.stdout);
    let mut out = String::new();
    if cut > 0 {
        out.push_str(&format!("[{cut} earlier lines cut]\n"));
    }
    out.push_str(kept);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("[{ended}]"));
    Ok(out)
}
