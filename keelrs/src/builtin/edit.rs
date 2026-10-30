//! `edit`: exact replacements in one file, all or none.

use std::fmt::Write as _;
use std::path::Path;

use rig_core::tool::{FileChange, HunkLine, ToolExecutionError, content_hash};
use rig_tools::edit::{Applied, apply};
use rig_tools::{Seen, clip};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Out, fail, key, place, read_text, text_of};
use crate::sandbox::{ContentHash, Sandbox};
use crate::{Tool, ToolCall};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditArgs {
    /// The file to edit, relative to the working directory.
    pub(crate) path: String,
    /// The replacements, in order: each matches the text the ones before it left.
    #[schemars(length(min = 1))]
    pub(crate) edits: Vec<Replacement>,
}

// One replacement of an `edit` call; not a doc comment, which the model would see.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Replacement {
    /// The text to replace, copied from the file; it must match one place only.
    pub(crate) old_text: String,
    /// The replacement.
    pub(crate) new_text: String,
    /// Replace every match instead of exactly one.
    #[serde(default)]
    pub(crate) replace_all: bool,
}

pub(super) fn tool(seen: Seen) -> Tool {
    Tool::sandbox(
        "edit",
        "Replace text in a file: each `old_text`, copied from the file, must match exactly one \
         place, unless `replace_all` is set, and replacements must not overlap. All are applied \
         or none. Make all changes to one file in one call, one entry per change. Read the \
         file first. Returns the changed lines with some context.",
        move |call: ToolCall<EditArgs>, sb: Sandbox| async move { edit(&sb, &seen, call.data).await },
    )
}

/// Makes `args.edits` to `args.path` as rig-tools' `edit` does: from the
/// text last seen whole, written only if the file still holds it, and
/// otherwise from the file as it is now.
pub(crate) async fn edit(sb: &Sandbox, seen: &Seen, args: EditArgs) -> Out {
    let sent = args.path.as_str();
    let path = place(sb, sent).await?;
    let key = key(&path);
    let edits: Vec<rig_tools::Replacement> = args
        .edits
        .iter()
        .map(|edit| rig_tools::Replacement {
            old_text: edit.old_text.clone(),
            new_text: edit.new_text.clone(),
            replace_all: edit.replace_all,
        })
        .collect();
    let mut held = None;
    if let Some((_, text)) = seen.text(&key)
        && let Ok(applied) = apply(sent, &text, &edits)
    {
        let hash = ContentHash::of(text.as_bytes());
        match sb
            .write_if(path.as_str(), applied.text.as_bytes(), hash)
            .await
        {
            Ok(None) => return Ok(reply(seen, &key, sent, edits.len(), &applied)),
            Ok(Some(now)) => held = Some(text_of(now, sent)?),
            Err(e) => {
                seen.forget(&key);
                return Err(fail(sent, e));
            }
        }
    }
    let text = match held {
        Some(text) => text,
        None => read_text(sb, &path, sent).await?,
    };
    let changed = seen
        .hash(&key)
        .is_some_and(|hash| hash != content_hash(text.as_bytes()));
    let applied = apply(sent, &text, &edits).map_err(|e| match changed {
        true => stale(sent, &e),
        false => e,
    })?;
    if let Err(e) = sb.write(path.as_str(), applied.text.as_bytes()).await {
        seen.forget(&key);
        return Err(fail(sent, e));
    }
    Ok(reply(seen, &key, sent, edits.len(), &applied))
}

/// `refusal` worded for a file `path` that changed since it was last read:
/// an `old_text` not found asks for a new read, without the closest text.
// Copied from rig-tools 0.46.0-rc.1 src/edit.rs (`not_found`): its
// `apply_to(.., changed)` is private, so `apply`'s refusal is reworded.
fn stale(path: &str, refusal: &ToolExecutionError) -> ToolExecutionError {
    let what = "`old_text` was not found, not even ignoring trailing whitespace";
    let changed = format!(
        "{what}: `{path}` changed since it was last read, so read it again and copy \
         `old_text` from what it holds now"
    );
    let message = refusal.to_string();
    let message = message
        .split("\nThe closest text")
        .next()
        .unwrap_or_default();
    ToolExecutionError::invalid_args(
        message
            .replace(
                &format!("{what}; copy it from the closest text in the file, shown below"),
                &changed,
            )
            .replace(
                &format!(
                    "{what}, and nothing in the file is close to it; check the path and the text"
                ),
                &changed,
            ),
    )
}

// Copied from rig-tools 0.46.0-rc.1 src/edit.rs (private there): the edit
// reply, from here to the end of the file.

const MAX_LISTED: usize = 8;
const MAX_CLOSEST_CHARS: usize = 300;
const CONTEXT: usize = 3;
const MAX_AROUND: usize = 60;
const MAX_PLACE: usize = 30;

fn reply(seen: &Seen, resolved: &Path, path: &str, edits: usize, applied: &Applied) -> String {
    seen.saw_whole(resolved, &applied.text);
    let places = places(&applied.change);
    let at = changed_at(&places, &applied.change, applied.replaced, edits);
    let mut out = format!("Edited {path}: {at}.");
    for note in &applied.notes {
        let _ = write!(out, "\nNote: {note}");
    }
    out.push_str(&around(&applied.text, &places));
    out
}

fn around(text: &str, places: &[Place]) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for place in places {
        let (first, last) = match *place {
            Place::Lines(first, last) => (first, last),
            Place::Removed(at) => (at, at.saturating_sub(1)),
        };
        let first = first.saturating_sub(CONTEXT).max(1);
        let last = (last + CONTEXT).min(lines.len());
        match runs.last_mut() {
            Some((_, end)) if first <= *end + 1 => *end = (*end).max(last),
            _ if first <= last => runs.push((first, last)),
            _ => {}
        }
    }
    let mut out = String::new();
    let mut shown = 0;
    for (index, &(first, last)) in runs.iter().enumerate() {
        if shown >= MAX_AROUND {
            let _ = write!(
                out,
                "\n[{} more changed places not shown; read them to see them]",
                runs.len() - index
            );
            break;
        }
        let _ = write!(out, "\n[lines {first}-{last}]");
        let count = last - first + 1;
        let half = MAX_PLACE / 2;
        for (number, line) in (first..=last).zip(lines.iter().skip(first - 1)) {
            if count > MAX_PLACE && number == first + half {
                let _ = write!(out, "\n[… {} lines]", count - 2 * half);
            }
            if count > MAX_PLACE && number >= first + half && number <= last - half {
                continue;
            }
            out.push('\n');
            out.push_str(clip(line, MAX_CLOSEST_CHARS));
            shown += 1;
        }
    }
    out
}

fn changed_at(places: &[Place], change: &FileChange, replaced: usize, edits: usize) -> String {
    let mut out = String::new();
    if replaced != edits {
        let _ = write!(out, "{replaced} replacements, ");
    }
    out.push_str(match places {
        [Place::Removed(_)] => "line ",
        [Place::Lines(first, last)] if first == last => "line ",
        [Place::Removed(_), ..] => "",
        _ => "lines ",
    });
    let listed: Vec<String> = places
        .iter()
        .take(MAX_LISTED)
        .map(|place| match *place {
            Place::Lines(first, last) if first == last => first.to_string(),
            Place::Lines(first, last) => format!("{first}-{last}"),
            Place::Removed(at) if places.len() == 1 => at.to_string(),
            Place::Removed(at) => format!("line {at}"),
        })
        .collect();
    out.push_str(&listed.join(", "));
    let more = places.len().saturating_sub(MAX_LISTED);
    if more > 0 {
        let _ = write!(out, ", and {more} more");
    }
    let shift = change.added as i64 - change.removed as i64;
    if shift != 0 {
        let noun = if shift.abs() == 1 { "line" } else { "lines" };
        let _ = write!(out, " ({shift:+} {noun})");
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Lines(usize, usize),
    Removed(usize),
}

fn places(change: &FileChange) -> Vec<Place> {
    let mut places = Vec::new();
    for hunk in &change.hunks {
        let kept = |line: &HunkLine| matches!(line, HunkLine::Same(_));
        let mut runs = hunk.lines.chunk_by(|a, b| kept(a) == kept(b)).peekable();
        let mut at = hunk.new.start;
        while let Some(run) = runs.next() {
            if run.first().is_some_and(kept) {
                at += run.len();
                continue;
            }
            let added = run.iter().filter(|line| line.sign() == '+').count();
            places.push(match added {
                0 if runs.peek().is_none() => Place::Removed(at.saturating_sub(1).max(1)),
                0 => Place::Removed(at),
                added => Place::Lines(at, at + added - 1),
            });
            at += added;
        }
    }
    places
}
