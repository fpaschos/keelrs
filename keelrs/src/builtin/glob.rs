//! `glob`: the files whose paths match a glob.

use schemars::JsonSchema;
use serde::Deserialize;

use rig_core::tool::ToolExecutionError;
use rig_tools::MAX_BYTES;

use super::{Out, fail, fit, listing, place, relative};
use crate::sandbox::{Sandbox, Stat};
use crate::{Tool, ToolCall};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GlobArgs {
    /// The path glob, such as `**/*.rs` or `src/*`.
    pub(crate) pattern: String,
    /// The directory to search in; default the working directory.
    pub(crate) path: Option<String>,
    /// Also hidden files and directories (named with a leading `.`); files git ignores stay out.
    #[serde(default)]
    pub(crate) hidden: bool,
}

/// Most paths one `glob` call returns.
const MAX_PATHS: usize = 1000;

pub(super) fn tool() -> Tool {
    Tool::sandbox(
        "glob",
        format!(
            "Find files by path glob, such as `**/*.rs` (at any depth) or `src/*` (directly in \
             src), from the working directory or `path`. Skips files ignored by git, target \
             directories and, unless hidden is set or the glob names one, hidden files. Returns \
             file paths relative to the working directory, sorted, at most {MAX_PATHS} or {} KB. \
             Use it to find files, not `find` or `ls` in `bash`.",
            MAX_BYTES / 1024
        ),
        |call: ToolCall<GlobArgs>, sb: Sandbox| async move { glob(&sb, call.data).await },
    )
}

/// The files under `args.path` whose paths below it match `args.pattern`.
pub(crate) async fn glob(sb: &Sandbox, args: GlobArgs) -> Out {
    let sent = args.path.as_deref().unwrap_or(".");
    let root = place(sb, sent).await?;
    let work_dir = place(sb, ".").await?;
    // Anchored at the root, so `*.md` is a path glob, not a name at any depth.
    let pattern = args.pattern.trim_start_matches("./");
    let pattern = match pattern.strip_prefix('/') {
        Some(_) => pattern.to_owned(),
        None => format!("/{pattern}"),
    };
    let listed = sb
        .list(root.as_str(), Some(&pattern), listing(args.hidden))
        .await
        .map_err(|e| fail(sent, e))?;
    let mut paths: Vec<String> = listed
        .iter()
        .map(|path| relative(&work_dir, path))
        .collect();
    paths.sort();
    let total = paths.len();
    // Only an empty listing asks whether the path is there at all.
    if total == 0 {
        if sb.stat(root.as_str()).await.map_err(|e| fail(sent, e))? == Stat::Missing {
            return Err(ToolExecutionError::not_found(format!(
                "{sent}: no such file or directory"
            )));
        }
        return Ok("No files found.".to_owned());
    }
    paths.truncate(MAX_PATHS);
    fit(&mut paths, MAX_BYTES);
    if paths.len() < total {
        paths.push(format!(
            "[showing {} of {total} paths; narrow the pattern or path]",
            paths.len()
        ));
    }
    Ok(paths.join("\n"))
}
