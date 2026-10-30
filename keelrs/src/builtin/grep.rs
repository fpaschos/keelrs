//! `grep`: file contents searched with a regular expression.

use regex::{Regex, RegexBuilder};
use rig_core::tool::ToolExecutionError;
use rig_tools::{MAX_FILE_BYTES, clip};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Out, fail, listing, place, relative};
use crate::sandbox::{Sandbox, Stat};
use crate::{Tool, ToolCall};

/// Most matches one `grep` call shows.
const MAX_MATCHES: usize = 100;
/// Characters a shown line is cut at.
const MAX_LINE: usize = 500;
/// Files read in one go, so a call holds a batch, never the whole tree.
const BATCH: usize = 64;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrepArgs {
    /// The regular expression (Rust regex syntax), or the text with `literal`.
    pub(crate) pattern: String,
    /// The directory or file to search; default the working directory.
    pub(crate) path: Option<String>,
    /// Only files matching this glob, such as `*.rs` or `src/**/*.rs`.
    pub(crate) glob: Option<String>,
    /// Match without regard to case.
    #[serde(default)]
    pub(crate) ignore_case: bool,
    /// Match `pattern` as plain text, not a regular expression.
    #[serde(default)]
    pub(crate) literal: bool,
    /// Lines to show before and after each match.
    pub(crate) context: Option<usize>,
    /// Also hidden files and directories (named with a leading `.`); files git ignores stay out.
    #[serde(default)]
    pub(crate) hidden: bool,
}

pub(super) fn tool() -> Tool {
    Tool::sandbox(
        "grep",
        format!(
            "Search file contents with a regular expression (Rust regex syntax), or plain text \
             with `literal`. Skips files ignored by git, target directories and, unless hidden \
             is set or the glob names one, hidden files. Returns `path:line: text` for each \
             match, at most {MAX_MATCHES}, with lines cut at {MAX_LINE} characters; with \
             `context`, the lines around each match as `path-line- text`, groups apart by \
             `--`. Paths are relative to the working directory, as `read` takes them. Use it \
             to find code, not `grep` or `rg` in `bash`."
        ),
        |call: ToolCall<GrepArgs>, sb: Sandbox| async move { grep(&sb, call.data).await },
    )
}

/// The lines matching `args.pattern` in the files under `args.path`, or in
/// that one file, in path order.
pub(crate) async fn grep(sb: &Sandbox, args: GrepArgs) -> Out {
    let pattern = match args.literal {
        true => regex::escape(&args.pattern),
        false => args.pattern.clone(),
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| ToolExecutionError::invalid_args(format!("bad pattern: {e}")))?;
    let whole = whole_text(&pattern, args.ignore_case);
    let sent = args.path.as_deref().unwrap_or(".");
    let root = place(sb, sent).await?;
    let work_dir = place(sb, ".").await?;
    let mut listed = match sb.stat(root.as_str()).await.map_err(|e| fail(sent, e))? {
        Stat::File { .. } => vec![root],
        Stat::Missing => {
            return Err(ToolExecutionError::not_found(format!(
                "{sent}: no such file or directory"
            )));
        }
        Stat::Dir | Stat::Other => sb
            .list(root.as_str(), args.glob.as_deref(), listing(args.hidden))
            .await
            .map_err(|e| fail(sent, e))?,
    };
    // All under the work dir, so in the order of the names shown.
    listed.sort();
    let context = args.context.unwrap_or(0);
    let mut out: Vec<String> = Vec::new();
    // `cut`: a match not shown, or a file not read, once `kept` is full.
    let (mut kept, mut cut) = (0, false);
    'files: for batch in listed.chunks(BATCH) {
        if kept == MAX_MATCHES {
            cut = true;
            break;
        }
        let texts = sb
            .read_files(batch, MAX_FILE_BYTES)
            .await
            .map_err(|e| fail(sent, e))?;
        for (path, bytes) in batch.iter().zip(texts) {
            if kept == MAX_MATCHES {
                cut = true;
                break 'files;
            }
            let name = relative(&work_dir, path);
            let Some(text) = bytes.and_then(|bytes| String::from_utf8(bytes).ok()) else {
                continue;
            };
            if let Some(whole) = &whole
                && !lone_cr(&text)
                && !whole.is_match(&text)
            {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            let hits: Vec<usize> = (0..lines.len())
                .filter(|&at| regex.is_match(lines[at]))
                .collect();
            cut |= hits.len() > MAX_MATCHES - kept;
            let hits = &hits[..hits.len().min(MAX_MATCHES - kept)];
            kept += hits.len();
            // Each hit with its context, overlapping or touching runs merged.
            let mut groups: Vec<(usize, usize)> = Vec::new();
            for &at in hits {
                let (first, last) = (
                    at.saturating_sub(context),
                    (at + context).min(lines.len() - 1),
                );
                match groups.last_mut() {
                    Some((_, end)) if first <= *end + 1 => *end = last.max(*end),
                    _ => groups.push((first, last)),
                }
            }
            for (first, last) in groups {
                if context > 0 && !out.is_empty() {
                    out.push("--".to_owned());
                }
                for (at, line) in lines.iter().enumerate().take(last + 1).skip(first) {
                    let mark = if hits.binary_search(&at).is_ok() {
                        ':'
                    } else {
                        '-'
                    };
                    out.push(format!(
                        "{name}{mark}{}{mark} {}",
                        at + 1,
                        clip(line, MAX_LINE)
                    ));
                }
            }
        }
    }
    if out.is_empty() {
        return Ok("No matches.".to_owned());
    }
    if cut {
        out.push(format!(
            "[showing the first {MAX_MATCHES} matches; narrow your search]"
        ));
    }
    Ok(out.join("\n"))
}

// Copied from rig-tools 0.46.0-rc.1 src/search.rs (private there), with
// `ignore_case`: a whole-text prefilter, so a file with no match is skipped
// without splitting it into lines.

/// `pattern` over a whole text, matching it wherever it matches one of its
/// lines, so a file it does not match has no line to search: `^` and `$`
/// match at each line end, `\n` or `\r\n`. `None` when the pattern anchors
/// to the ends of the text (`\A`, `\z`) or may turn a flag off, such as
/// `(?-m)`, so its lines' ends would not count.
fn whole_text(pattern: &str, ignore_case: bool) -> Option<Regex> {
    let groups = pattern.split("(?").skip(1);
    let mut flags = groups.filter_map(|group| group.split([')', ':']).next());
    if pattern.contains("\\A") || pattern.contains("\\z") || flags.any(|it| it.contains('-')) {
        return None;
    }
    RegexBuilder::new(pattern)
        .multi_line(true)
        .crlf(true)
        .case_insensitive(ignore_case)
        .build()
        .ok()
}

/// Whether `text` has a `\r` that does not end a line: [`whole_text`]'s
/// `.` does not match it, though a line's does.
fn lone_cr(text: &str) -> bool {
    text.contains('\r') && text.matches('\r').count() != text.matches("\r\n").count()
}
