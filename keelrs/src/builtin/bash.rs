//! `bash`: a command line in the sandbox's shell.

use std::borrow::Cow;
use std::time::Duration;

use rig_core::redact;
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
         {} MB, unless `timeout` (seconds) is given. Terminal escape codes are dropped and \
         secrets it prints, such as API keys and private keys, show as [REDACTED]; `read` \
         shows a file exactly. Use `read`, `grep`, `glob`, `edit` and `write` for files, not `cat`, \
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
    tail.finish();
    let exit = execution.wait().await.map_err(failed)?;
    let ended = match exit {
        _ if stopped => format!("stopped after printing {} MB", MAX_PRINTED / (1024 * 1024)),
        Exit::Code(code) => format!("exit code {code}"),
        Exit::Signaled(signal) => {
            let (name, why) = sent(signal);
            format!("ended by {name} (signal {signal}): {why}")
        }
        Exit::TimedOut => format!(
            "timed out after {}s and was stopped{}",
            args.timeout.unwrap_or_default(),
            if tail.printed == 0 { SILENT } else { "" }
        ),
        Exit::Killed => "killed".to_owned(),
    };
    let mut out = String::new();
    match OutputLimit::default().tail(&tail.kept) {
        ("", _) if !tail.kept.is_empty() => {
            let start = tail
                .kept
                .ceil_char_boundary(tail.kept.len().saturating_sub(MAX_BYTES));
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

// Copied from rig-tools 0.46.0-rc.1 src/shell.rs (`SILENT`, `sent`,
// `plain`; private there).

/// `line` without the terminal escape sequences in it, such as the colours
/// a program prints even when its output is no terminal, or a link's or a
/// window title's markup: the text between them stays.
fn plain(line: &str) -> Cow<'_, str> {
    if !line.contains('\x1b') {
        return Cow::Borrowed(line);
    }
    let mut text = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            text.push(c);
            continue;
        }
        match chars.next() {
            // A control sequence: parameters, then one final character.
            Some('[') => {
                let _ = chars.by_ref().find(|c| ('\x40'..='\x7e').contains(c));
            }
            // A string, such as a link or a title, up to BEL or `ESC \`.
            Some(']' | 'P' | 'X' | '^' | '_') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            // Intermediate characters, then one final character.
            Some(' '..='/') => {
                let _ = chars.by_ref().find(|c| !(' '..='/').contains(c));
            }
            _ => {}
        }
    }
    Cow::Owned(text)
}

/// What a timeout adds when the command printed nothing at all.
const SILENT: &str = ". It printed nothing: a program whose output is not a terminal may hold \
                      it back until it ends, so make it print as it goes (such as \
                      `python3 -u` or `stdbuf -oL`) to see how far it got";

/// The name of `signal` and what likely sent it.
fn sent(signal: i32) -> (&'static str, &'static str) {
    match signal {
        libc::SIGHUP => ("SIGHUP", "its terminal or session went away"),
        libc::SIGINT => ("SIGINT", "something on this machine interrupted it"),
        libc::SIGQUIT => ("SIGQUIT", "something on this machine asked it to quit"),
        libc::SIGILL => ("SIGILL", "it crashed"),
        libc::SIGTRAP => ("SIGTRAP", "it crashed, or a debugger stopped it"),
        libc::SIGABRT => ("SIGABRT", "it aborted itself, as on a failed assertion"),
        libc::SIGBUS => ("SIGBUS", "it crashed"),
        libc::SIGFPE => ("SIGFPE", "it crashed on an arithmetic error"),
        libc::SIGKILL => (
            "SIGKILL",
            "something on this machine killed it, such as the out-of-memory killer or a `kill`",
        ),
        libc::SIGSEGV => ("SIGSEGV", "it crashed"),
        libc::SIGPIPE => ("SIGPIPE", "it wrote to a pipe that nothing read any more"),
        libc::SIGTERM => ("SIGTERM", "something on this machine asked it to stop"),
        _ => ("a signal", "something on this machine sent it"),
    }
}

/// Most output one `bash` call lets a command print before stopping it, as
/// rig-tools' shell does.
const MAX_PRINTED: usize = 10 * 1024 * 1024;

/// The longest unfinished line held for its end; past it, a piece is taken
/// as it is, as rig-tools' shell does (`MAX_HELD`).
const MAX_HELD: usize = 1024 * 1024;

/// The end of a command's output as the model sees it, line by line: no
/// terminal escape sequences ([`plain`]) and secrets redacted; enough for
/// [`OutputLimit::tail`], at least the last [`MAX_BYTES`] and at most twice
/// that.
#[derive(Default)]
struct Tail {
    kept: String,
    /// Lines ended in what was dropped from the front of `kept`.
    cut: usize,
    /// Bytes printed in all.
    printed: usize,
    /// The line printed so far, not yet ended.
    held: String,
    /// Inside a private key block, left out through its end line.
    in_key: bool,
}

impl Tail {
    fn push(&mut self, text: &str) {
        self.printed += text.len();
        let mut held = std::mem::take(&mut self.held);
        held.push_str(text);
        let mut start = 0;
        while let Some(at) = held[start..].find('\n') {
            self.line(&held[start..=start + at]);
            start += at + 1;
        }
        self.held = held.split_off(start);
        if self.held.len() > MAX_HELD {
            let piece = std::mem::take(&mut self.held);
            self.line(&piece);
        }
    }

    /// Takes the line left unfinished when the output ended.
    fn finish(&mut self) {
        let piece = std::mem::take(&mut self.held);
        if !piece.is_empty() {
            self.line(&piece);
        }
    }

    fn line(&mut self, piece: &str) {
        let piece = plain(piece);
        let piece = self.redact(&piece);
        self.keep(&piece);
    }

    // Copied from rig-tools 0.46.0-rc.1 src/shell.rs (`Kept::redact`).
    /// `piece`, a line, with its secrets redacted ([`redact::text`]). A
    /// private key block spans lines: its first is redacted to its end, and
    /// the lines after it, through its end line, are left out.
    fn redact<'a>(&mut self, piece: &'a str) -> Cow<'a, str> {
        if self.in_key {
            self.in_key = !piece.contains("-----END ");
            return Cow::Borrowed("");
        }
        let mut redacted = redact::text(piece);
        if let Cow::Owned(line) = &mut redacted {
            self.in_key = redact::find(piece).any(|secret| {
                secret.kind == redact::Kind::PrivateKey
                    && piece
                        .get(secret.range)
                        .is_some_and(|key| !key.contains("-----END "))
            });
            // A key redacted to the line's end took its newline.
            if piece.ends_with('\n') && !line.ends_with('\n') {
                line.push('\n');
            }
        }
        redacted
    }

    fn keep(&mut self, text: &str) {
        self.kept.push_str(text);
        if self.kept.len() > 2 * MAX_BYTES {
            let drop = self.kept.ceil_char_boundary(self.kept.len() - MAX_BYTES);
            self.cut += self.kept[..drop].matches('\n').count();
            self.kept.drain(..drop);
        }
    }
}
