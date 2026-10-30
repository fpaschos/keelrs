//! `delete` and `move`: whole files, never directories.

use rig_core::tool::ToolExecutionError;
use rig_tools::Seen;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Out, fail, key, place};
use crate::sandbox::{Sandbox, SandboxPath, Stat};
use crate::{Tool, ToolCall};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteArgs {
    /// The file to delete.
    pub(crate) path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoveArgs {
    /// The file to move.
    pub(crate) from: String,
    /// Its new path.
    pub(crate) to: String,
    /// Replace a file already at `to`.
    #[serde(default)]
    pub(crate) overwrite: bool,
}

pub(super) fn delete_tool(seen: Seen) -> Tool {
    Tool::sandbox(
        "delete",
        "Delete a file, not with `rm` in `bash`. Directories are refused.",
        move |call: ToolCall<DeleteArgs>, sb: Sandbox| async move {
            delete(&sb, &seen, call.data).await
        },
    )
}

pub(super) fn move_tool(seen: Seen) -> Tool {
    Tool::sandbox(
        "move",
        "Move or rename a file, not with `mv` in `bash`, creating the new path's directory. \
         Refuses directories, and an existing file at `to` unless `overwrite` is true.",
        move |call: ToolCall<MoveArgs>, sb: Sandbox| async move {
            move_file(&sb, &seen, call.data).await
        },
    )
}

/// Removes the file `args.path`, which `seen` then forgets.
// The rules and wording are rig-tools 0.46.0-rc.1 src/delete.rs's.
pub(crate) async fn delete(sb: &Sandbox, seen: &Seen, args: DeleteArgs) -> Out {
    let shown = args.path.as_str();
    let path = place(sb, shown).await?;
    require_file(sb, &path, shown, "`delete` removes").await?;
    seen.forget(&key(&path));
    sb.remove(path.as_str(), false)
        .await
        .map_err(|e| fail(shown, e))?;
    Ok(format!("Deleted {shown}."))
}

/// Moves the file `args.from` to `args.to`; what `seen` remembers of it moves too.
// The rules and wording are rig-tools 0.46.0-rc.1 src/move_file.rs's.
pub(crate) async fn move_file(sb: &Sandbox, seen: &Seen, args: MoveArgs) -> Out {
    let (shown_from, shown_to) = (args.from.as_str(), args.to.as_str());
    let from = place(sb, shown_from).await?;
    let to = place(sb, shown_to).await?;
    require_file(sb, &from, shown_from, "`move` moves").await?;
    if from == to {
        return Err(ToolExecutionError::invalid_args(format!(
            "{shown_from} and {shown_to} are the same file"
        )));
    }
    match sb.stat(to.as_str()).await.map_err(|e| fail(shown_to, e))? {
        Stat::Missing => {}
        Stat::File { .. } if args.overwrite => {}
        Stat::File { .. } => {
            return Err(ToolExecutionError::invalid_args(format!(
                "{shown_to} already exists; set `overwrite` to replace it"
            )));
        }
        Stat::Dir | Stat::Other => {
            return Err(ToolExecutionError::invalid_args(format!(
                "{shown_to} is not a regular file; give the file's new path"
            )));
        }
    }
    let (from_key, to_key) = (key(&from), key(&to));
    let moved = seen.text(&from_key);
    let hash = seen.hash(&from_key);
    seen.forget(&from_key);
    seen.forget(&to_key);
    sb.rename(from.as_str(), to.as_str())
        .await
        .map_err(|e| fail(shown_from, e))?;
    match (moved, hash) {
        (Some((_, text)), _) => seen.saw_whole(&to_key, &text),
        (None, Some(hash)) => seen.saw(&to_key, hash),
        (None, None) => {}
    }
    Ok(format!("Moved {shown_from} to {shown_to}."))
}

/// Refuses `path`, which the model calls `shown`, unless it is a regular
/// file; `tool` says what takes files only, as in "`delete` removes".
async fn require_file(
    sb: &Sandbox,
    path: &SandboxPath,
    shown: &str,
    tool: &str,
) -> Result<(), ToolExecutionError> {
    match sb.stat(path.as_str()).await.map_err(|e| fail(shown, e))? {
        Stat::File { .. } => Ok(()),
        Stat::Missing => Err(ToolExecutionError::not_found(format!(
            "{shown}: no such file"
        ))),
        Stat::Dir | Stat::Other => Err(ToolExecutionError::invalid_args(format!(
            "{shown} is not a regular file; {tool} files only"
        ))),
    }
}
