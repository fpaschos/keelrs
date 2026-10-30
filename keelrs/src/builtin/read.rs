//! `read`: a text file's lines, paged.

use rig_tools::{MAX_BYTES, MAX_FILE_BYTES, MAX_LINES, Seen};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Out, key, place, read_text};
use crate::sandbox::Sandbox;
use crate::{Tool, ToolCall};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadArgs {
    /// The file to read, relative to the working directory.
    pub(crate) path: String,
    /// The first line to read, from 1.
    pub(crate) offset: Option<usize>,
    /// How many lines to read.
    pub(crate) limit: Option<usize>,
}

pub(super) fn tool(seen: Seen) -> Tool {
    Tool::sandbox(
        "read",
        format!(
            "Read a text file of at most {} MB: its lines as they are, at most {MAX_LINES} lines \
             or {} KB, then the offset to read on from if it stopped early. Use offset (from 1) \
             and limit for part of a file. Use `read` to look at a file, not `cat`, `head` or \
             `sed` in `bash`, and read several files at once by calling it several times in \
             one reply.",
            MAX_FILE_BYTES / (1024 * 1024),
            MAX_BYTES / 1024
        ),
        move |call: ToolCall<ReadArgs>, sb: Sandbox| async move { read(&sb, &seen, call.data).await },
    )
}

/// `args.path`'s lines as rig-tools' `read` shows them; its whole text is
/// remembered in `seen` for a later `edit`.
pub(crate) async fn read(sb: &Sandbox, seen: &Seen, args: ReadArgs) -> Out {
    let path = place(sb, &args.path).await?;
    let text = read_text(sb, &path, &args.path).await?;
    let mut page = rig_tools::ReadArgs::new(args.path);
    page.offset = args.offset;
    page.limit = args.limit;
    seen.saw_whole(&key(&path), &text);
    Ok(page.show(&text))
}
