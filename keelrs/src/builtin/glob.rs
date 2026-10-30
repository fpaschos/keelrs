//! `glob`: the files whose paths match a glob.

use schemars::JsonSchema;
use serde::Deserialize;

use super::{Out, fail, place, relative};
use crate::sandbox::{Listing, Sandbox};
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
             file paths relative to the working directory, sorted, at most {MAX_PATHS}. Use it \
             to find files, not `find` or `ls` in `bash`."
        ),
        |call: ToolCall<GlobArgs>, sb: Sandbox| async move { glob(&sb, call.data).await },
    )
}

/// The files under `args.path` whose paths below it match `args.pattern`.
pub(crate) async fn glob(sb: &Sandbox, args: GlobArgs) -> Out {
    let shown = args.path.as_deref().unwrap_or(".");
    let root = place(sb, shown).await?;
    let work_dir = place(sb, ".").await?;
    // Anchored at the root, so `*.md` is a path glob, not a name at any depth.
    let pattern = match args.pattern.strip_prefix('/') {
        Some(_) => args.pattern.clone(),
        None => format!("/{}", args.pattern),
    };
    let listing = if args.hidden {
        Listing::Hidden
    } else {
        Listing::Visible
    };
    let listed = sb
        .list(root.as_str(), Some(&pattern), listing)
        .await
        .map_err(|e| fail(shown, e))?;
    let mut paths: Vec<String> = listed
        .iter()
        .map(|path| relative(&work_dir, path))
        .collect();
    paths.sort();
    let total = paths.len();
    if total == 0 {
        return Ok("No files found.".to_owned());
    }
    paths.truncate(MAX_PATHS);
    if total > MAX_PATHS {
        paths.push(format!(
            "[showing {MAX_PATHS} of {total} paths; narrow the pattern or path]"
        ));
    }
    Ok(paths.join("\n"))
}
