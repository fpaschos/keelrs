//! The built-in tools, `read`, `write`, `edit`, `bash`, `grep`, `glob`,
//! `delete` and `move`: sandboxed [`Tool`]s that every turn declaring a
//! sandbox gets, unless it narrows them with
//! [`only`](crate::agent::BuiltinTools::only). They are built each render
//! over that render's sandbox, as the skill tools are, and act only through
//! [`Sandbox`]'s own methods, so they behave alike on every backend.
//!
//! They reuse rig-tools' text logic (`Seen`, `ReadArgs::shown`,
//! `edit::apply`, `clip`); what rig-tools keeps private is copied, marked
//! where it is. No Rig type leaves this module (ADR 0004, rule 2).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use rig_core::tool::ToolExecutionError;
use rig_tools::{MAX_FILE_BYTES, Seen};

use crate::Tool;
use crate::sandbox::{Sandbox, SandboxError, SandboxId, SandboxPath};

mod bash;
mod edit;
mod files;
mod glob;
mod grep;
mod read;
mod write;

#[cfg(test)]
mod tests;

/// The built-in tools' names, in the order the model sees them.
pub(crate) const NAMES: [&str; 8] = [
    "read", "write", "edit", "bash", "grep", "glob", "delete", "move",
];

/// What the built-in tools remember for one agent instance, per sandbox:
/// what the model last read or left in each file. In memory only, and
/// correct when empty, so losing it costs only a fresh read.
#[derive(Debug, Default)]
pub(crate) struct Caches(Mutex<HashMap<SandboxId, Seen>>);

impl Caches {
    /// What this instance has seen in the sandbox `id`.
    pub(crate) fn seen(&self, id: SandboxId) -> Seen {
        let mut caches = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        caches.entry(id).or_default().clone()
    }
}

/// The built-in tools `names` (from [`NAMES`], in its order) over
/// `sandbox`, sharing `seen`.
pub(crate) fn tools(sandbox: &Sandbox, seen: &Seen, names: &[&str]) -> Vec<Tool> {
    names
        .iter()
        .map(|name| match *name {
            "read" => read::tool(seen.clone()),
            "write" => write::tool(seen.clone()),
            "edit" => edit::tool(seen.clone()),
            "bash" => bash::tool(sandbox.shell()),
            "grep" => grep::tool(),
            "glob" => glob::tool(),
            "delete" => files::delete_tool(seen.clone()),
            "move" => files::move_tool(seen.clone()),
            other => unreachable!("`{other}` is not a built-in tool"),
        })
        .collect()
}

/// What a built-in tool's call settles to.
type Out = Result<String, ToolExecutionError>;

/// `path` from the work dir, canonical; a refusal names `path` as the model sent it.
async fn place(sb: &Sandbox, path: &str) -> Result<SandboxPath, ToolExecutionError> {
    sb.canonical(path).await.map_err(|e| fail(path, e))
}

/// The key a canonical path has in [`Seen`].
fn key(path: &SandboxPath) -> PathBuf {
    PathBuf::from(path.as_str())
}

/// `path`, canonical, as the model names it: relative to the canonical
/// `work_dir`, as `read` takes it.
fn relative(work_dir: &SandboxPath, path: &SandboxPath) -> String {
    match path.as_str().strip_prefix(work_dir.as_str()) {
        Some(below) => below.trim_start_matches('/').to_owned(),
        None => path.to_string(),
    }
}

/// The whole UTF-8 text of `path`, which the model calls `shown`; refused
/// over [`MAX_FILE_BYTES`].
async fn whole_text(
    sb: &Sandbox,
    path: &SandboxPath,
    shown: &str,
) -> Result<String, ToolExecutionError> {
    let contents = sb
        .read(path.as_str(), MAX_FILE_BYTES + 1)
        .await
        .map_err(|e| fail(shown, e))?;
    if contents.size > MAX_FILE_BYTES {
        return Err(ToolExecutionError::invalid_args(format!(
            "{shown} is larger than {} MB",
            MAX_FILE_BYTES / (1024 * 1024)
        )));
    }
    String::from_utf8(contents.bytes).map_err(|_| not_text(shown))
}

/// A sandbox error worded for the model, naming `shown`, the path it sent.
// Copied from rig-tools 0.46.0-rc.1 src/workspace.rs (`text_error`), over keelrs's errors.
fn fail(shown: &str, error: crate::Error) -> ToolExecutionError {
    use std::io::ErrorKind;
    let crate::Error::Sandbox(error) = error else {
        return ToolExecutionError::other(error.to_string());
    };
    match error {
        SandboxError::Io {
            kind: ErrorKind::NotFound,
            ..
        } => ToolExecutionError::not_found(format!("{shown}: no such file")),
        SandboxError::Io {
            kind: ErrorKind::InvalidInput | ErrorKind::IsADirectory,
            ..
        } => ToolExecutionError::invalid_args(format!("{shown} is not a regular file")),
        SandboxError::OutsideWorkDir { .. } => {
            ToolExecutionError::invalid_args(format!("{shown} is outside the work dir"))
        }
        SandboxError::Io { message, .. } => {
            ToolExecutionError::other(format!("{shown}: {message}"))
        }
        other => ToolExecutionError::other(other.to_string()),
    }
}

fn not_text(shown: &str) -> ToolExecutionError {
    ToolExecutionError::invalid_args(format!("{shown} is not UTF-8 text"))
}
