//! `write`: a file's whole content, created or replaced.

use rig_tools::{MAX_FILE_BYTES, Seen};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Out, fail, key, place};
use crate::sandbox::{Sandbox, Stat};
use crate::{Tool, ToolCall};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteArgs {
    /// The file to write, relative to the working directory.
    pub(crate) path: String,
    /// The file's whole new content.
    pub(crate) content: String,
}

pub(super) fn tool(seen: Seen) -> Tool {
    Tool::sandbox(
        "write",
        "Create a file, or replace one's whole content, creating its directory. To change part \
         of a file, use `edit`.",
        move |call: ToolCall<WriteArgs>, sb: Sandbox| async move { write(&sb, &seen, call.data).await },
    )
}

/// Writes `args.content` to `args.path`, remembered in `seen`; says whether
/// it created the file or how long the one it replaced was.
pub(crate) async fn write(sb: &Sandbox, seen: &Seen, args: WriteArgs) -> Out {
    let path = place(sb, &args.path).await?;
    let before = match sb
        .stat(path.as_str())
        .await
        .map_err(|e| fail(&args.path, e))?
    {
        Stat::File { size, .. } if size <= MAX_FILE_BYTES => {
            let contents = sb
                .read(path.as_str(), MAX_FILE_BYTES)
                .await
                .map_err(|e| fail(&args.path, e))?;
            match String::from_utf8(contents.bytes) {
                Ok(text) => Before::Lines(text.lines().count()),
                Err(_) => Before::Uncounted,
            }
        }
        Stat::Missing => Before::Nothing,
        _ => Before::Uncounted,
    };
    if let Err(e) = sb.write(path.as_str(), args.content.as_bytes()).await {
        // It may or may not have been written: the next edit reads the file.
        seen.forget(&key(&path));
        return Err(fail(&args.path, e));
    }
    seen.saw_whole(&key(&path), &args.content);
    Ok(wrote(&args.path, before))
}

/// What a `write` replaced.
#[derive(Clone, Copy)]
enum Before {
    Nothing,
    Lines(usize),
    /// A file whose lines were not counted: too large, or not UTF-8.
    Uncounted,
}

/// `write`'s reply for `path`, which held `before`.
// Copied from rig-tools 0.46.0-rc.1 src/write.rs (`wrote`), over `Before`.
fn wrote(path: &str, before: Before) -> String {
    match before {
        Before::Nothing => format!("Created {path}."),
        Before::Lines(1) => format!("Replaced {path}, which had 1 line."),
        Before::Lines(lines) => format!("Replaced {path}, which had {lines} lines."),
        Before::Uncounted => format!("Replaced {path}."),
    }
}
