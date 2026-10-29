//! PROTOTYPE, never merged (#53): `read`, `write`, `edit`, `search` and `list` as thin async
//! tools over [`Sandbox`], reusing rig-tools' public helpers. No Rig `Workspace` reaches the
//! host and no thread is started here; what each tool could not reuse is marked COPIED or
//! REWRITTEN.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use keelrs::sandbox::{ContentHash, Sandbox, SandboxPath};
use keelrs::{Tool, ToolCall};
use rig_core::tool::{FileChange, HunkLine, ToolExecutionError, content_hash};
use rig_tools::process::{Launcher, Process, Spawn};
use rig_tools::{SearchArgs, find, render};
use rig_tools::workspace::{Contents, Listing, Stat, Workspace};
use rig_tools::{
    EditArgs, ListArgs, MAX_BYTES, MAX_FILE_BYTES, MAX_LINES, ReadArgs, Seen, WriteArgs, clip,
    edit::apply,
};

type Out = Result<String, ToolExecutionError>;

/// The five tools, sharing one [`Seen`], as keelrs sandboxed tools.
pub fn tools(seen: &Seen) -> Vec<Tool> {
    let (r, w, e) = (seen.clone(), seen.clone(), seen.clone());
    vec![
        Tool::sandbox(
            "read",
            "…",
            move |c: ToolCall<ReadArgs>, sb: Sandbox| async move { read(&sb, &r, c.data).await },
        ),
        Tool::sandbox(
            "write",
            "…",
            move |c: ToolCall<WriteArgs>, sb: Sandbox| async move { write(&sb, &w, c.data).await },
        ),
        Tool::sandbox(
            "edit",
            "…",
            move |c: ToolCall<EditArgs>, sb: Sandbox| async move { edit(&sb, &e, c.data).await },
        ),
        Tool::sandbox(
            "search",
            "…",
            |c: ToolCall<SearchArgs>, sb: Sandbox| async move { search(&sb, c.data).await },
        ),
        Tool::sandbox(
            "list",
            "…",
            |c: ToolCall<ListArgs>, sb: Sandbox| async move { list(&sb, c.data).await },
        ),
    ]
}

// ---- read: fully reused (ReadArgs::shown, Seen) ----------------------------------------------

pub async fn read(sb: &Sandbox, seen: &Seen, args: ReadArgs) -> Out {
    let path = place(sb, &args.path).await?;
    let text = whole_text(sb, &path).await?;
    let (shown, _lines) = args.shown(&text);
    seen.saw_whole(&key(&path), &text);
    Ok(shown)
}

// ---- write: reply REWRITTEN (rig's `wrote` and `Previous` are private) -----------------------

pub async fn write(sb: &Sandbox, seen: &Seen, args: WriteArgs) -> Out {
    let path = place(sb, &args.path).await?;
    let before = sb.read(path.as_str(), MAX_FILE_BYTES).await.ok();
    sb.write(path.as_str(), args.content.as_bytes())
        .await
        .map_err(|e| {
            seen.forget(&key(&path));
            fail(e)
        })?;
    seen.saw_whole(&key(&path), &args.content);
    Ok(match before.map(|b| String::from_utf8(b.bytes).ok()) {
        None => format!("Created {}.", args.path),
        Some(Some(t)) if t.lines().count() == 1 => {
            format!("Replaced {}, which had 1 line.", args.path)
        }
        Some(Some(t)) => format!(
            "Replaced {}, which had {} lines.",
            args.path,
            t.lines().count()
        ),
        Some(None) => format!("Replaced {}.", args.path),
    })
}

// ---- edit: matching reused (edit::apply, Seen); flow REWRITTEN; reply COPIED -----------------

pub async fn edit(sb: &Sandbox, seen: &Seen, args: EditArgs) -> Out {
    let path = place(sb, &args.path).await?;
    let (key, shown) = (key(&path), path.as_str());
    let mut held = None;
    if let Some((_, text)) = seen.text(&key)
        && let Ok(applied) = apply(shown, &text, &args.edits)
    {
        let now = sb
            .write_if(
                path.as_str(),
                applied.text.as_bytes(),
                ContentHash::of(text.as_bytes()),
            )
            .await
            .map_err(|e| {
                seen.forget(&key);
                fail(e)
            })?;
        match now {
            None => return Ok(reply(seen, &key, &args, &applied)),
            Some(now) => held = Some(String::from_utf8(now.bytes).map_err(|_| not_text(shown))?),
        }
    }
    let raw = match held {
        Some(raw) => raw,
        None => whole_text(sb, &path).await?,
    };
    let changed = seen
        .hash(&key)
        .is_some_and(|hash| hash != content_hash(raw.as_bytes()));
    // `apply_to(.., changed)` is private: the "changed since it was last read" hint is lost.
    let applied = apply(shown, &raw, &args.edits).map_err(|e| match changed {
        true => ToolExecutionError::invalid_args(format!(
            "{e} (`{shown}` changed since it was last read, so read it again)"
        )),
        false => e,
    })?;
    sb.write(path.as_str(), applied.text.as_bytes())
        .await
        .map_err(|e| {
            seen.forget(&key);
            fail(e)
        })?;
    Ok(reply(seen, &key, &args, &applied))
}

// ---- search: find/render reused over an in-memory snapshot `Workspace` -----------------------

pub async fn search(sb: &Sandbox, mut args: SearchArgs) -> Out {
    let base = place(sb, ".").await?;
    let root = place(sb, args.path.as_deref().unwrap_or(".")).await?;
    let listed = visible(sb, &root, args.glob.as_deref(), args.listing()).await?;
    let paths: Vec<PathBuf> = listed.iter().map(key).collect();
    let bytes = match args.files && args.pattern.is_empty() {
        true => vec![None; paths.len()],
        false => sb.read_files(&listed.iter().map(SandboxPath::as_str).collect::<Vec<_>>(), MAX_FILE_BYTES).await.map_err(fail)?,
    };
    let snapshot = Snapshot(paths.into_iter().zip(bytes).collect());
    let relative = args
        .path
        .as_deref()
        .is_none_or(|p| Path::new(p).is_relative());
    args.path = Some(root.as_str().to_owned());
    let mut found = find(&snapshot, &args)?; // in memory: no thread, no wait
    if relative {
        let base = key(&base);
        for path in found
            .hits
            .iter_mut()
            .map(|h| &mut h.path)
            .chain(&mut found.names)
        {
            if let Ok(below) = path.strip_prefix(&base) {
                *path = below.to_path_buf();
            }
        }
    }
    Ok(render(&found))
}

/// The listed files and their pre-read bytes: just enough `Workspace` for `search::find`.
#[derive(Debug)]
struct Snapshot(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Workspace for Snapshot {
    fn files(&self, _: &Path, _: Option<&str>, _: Listing) -> io::Result<Vec<PathBuf>> {
        Ok(self.0.iter().map(|(p, _)| p.clone()).collect())
    }
    fn read_files(&self, paths: &[PathBuf], _: u64) -> Vec<Option<Vec<u8>>> {
        let by: HashMap<_, _> = self.0.iter().map(|(p, b)| (p, b)).collect();
        paths
            .iter()
            .map(|p| by.get(p).and_then(|b| (*b).clone()))
            .collect()
    }
    fn canonical(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }
    fn stat(&self, _: &Path) -> io::Result<Stat> {
        Ok(Stat::Dir) // only asked when the listing is empty: the root exists, `place` checked
    }
    fn read(&self, _: &Path, _: u64) -> io::Result<Contents> {
        unreachable!("find reads through read_files")
    }
    fn write(&self, _: &Path, _: &[u8]) -> io::Result<()> {
        unreachable!()
    }
    fn remove(&self, _: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
        unreachable!()
    }
    fn spawn(
        &self,
        _: &Launcher,
        _: Option<&Path>,
        _: &OsStr,
        _: &[OsString],
        _: Spawn,
    ) -> io::Result<Process> {
        unreachable!()
    }
}

// ---- list: REWRITTEN (rig's `list` is private) ------------------------------------------------

pub async fn list(sb: &Sandbox, args: ListArgs) -> Out {
    let root = place(sb, args.path.as_deref().unwrap_or(".")).await?;
    let files = visible(sb, &root, args.glob.as_deref(), args.listing()).await?;
    let depth = args.depth.unwrap_or(usize::MAX).max(1);
    let entries: BTreeSet<String> = files
        .iter()
        .filter_map(|file| {
            let below = key(file).strip_prefix(key(&root)).ok()?.to_path_buf();
            let parts: Vec<_> = below.iter().collect();
            Some(match parts.get(..depth) {
                Some(kept) if kept.len() < parts.len() => {
                    format!("{}/", kept.iter().collect::<PathBuf>().display())
                }
                _ if below.as_os_str().is_empty() => file.as_str().to_owned(),
                _ => below.to_string_lossy().into_owned(),
            })
        })
        .collect();
    let total = entries.len();
    let mut text = String::new();
    let mut kept = 0;
    for entry in entries {
        if kept == MAX_LINES || text.len() + entry.len() + 1 > MAX_BYTES {
            break;
        }
        text.push_str(&entry);
        text.push('\n');
        kept += 1;
    }
    if kept < total {
        let _ = writeln!(
            text,
            "[{} more entries; narrow the listing with path, depth or glob]",
            total - kept
        );
    }
    if total == 0 {
        text.push_str("No files.\n");
    }
    Ok(text)
}

/// `Sandbox::list` lists every file (rig's `Listing::All`): this filters hidden and `target`
/// paths like `Listing::Visible`/`Hidden`, but cannot read `.gitignore`.
async fn visible(
    sb: &Sandbox,
    root: &SandboxPath,
    glob: Option<&str>,
    listing: Listing,
) -> Result<Vec<SandboxPath>, ToolExecutionError> {
    let all = sb.list(root.as_str(), glob).await.map_err(fail)?;
    let root = key(root);
    Ok(all
        .into_iter()
        .filter(|p| {
            let below = key(p);
            let below = below.strip_prefix(&root).unwrap_or(&below);
            below.iter().all(|part| {
                let part = part.to_string_lossy();
                match listing {
                    Listing::All => true,
                    Listing::Hidden => part != ".git",
                    Listing::Visible => !part.starts_with('.') && part != "target",
                }
            })
        })
        .collect())
}

// ---- shared ------------------------------------------------------------------------------------

async fn place(sb: &Sandbox, path: &str) -> Result<SandboxPath, ToolExecutionError> {
    sb.canonical(path).await.map_err(fail)
}

/// The whole UTF-8 text of `path`, refused over rig's `MAX_FILE_BYTES` as `read_text` does.
async fn whole_text(sb: &Sandbox, path: &SandboxPath) -> Result<String, ToolExecutionError> {
    let contents = sb.read(path.as_str(), MAX_FILE_BYTES + 1).await.map_err(fail)?;
    if contents.size > MAX_FILE_BYTES {
        return Err(ToolExecutionError::invalid_args(format!(
            "{path} is too large"
        )));
    }
    String::from_utf8(contents.bytes).map_err(|_| not_text(path.as_str()))
}

fn key(path: &SandboxPath) -> PathBuf {
    PathBuf::from(path.as_str())
}

/// REWRITTEN (rig's `text_error` is private): a sandbox error worded for the model.
fn fail(error: keelrs::Error) -> ToolExecutionError {
    use keelrs::sandbox::SandboxError::Io;
    match error {
        keelrs::Error::Sandbox(Io { path, kind: std::io::ErrorKind::NotFound, .. }) => {
            ToolExecutionError::not_found(format!("{path}: no such file"))
        }
        error => ToolExecutionError::other(error.to_string()),
    }
}

fn not_text(path: &str) -> ToolExecutionError {
    ToolExecutionError::invalid_args(format!("{path} is not UTF-8 text"))
}

// ---- COPIED from rig-tools src/edit.rs (private there): the edit reply -------------------------

const MAX_LISTED: usize = 8;
const MAX_CLOSEST_CHARS: usize = 300;
const CONTEXT: usize = 3;
const MAX_AROUND: usize = 60;
const MAX_PLACE: usize = 30;

fn reply(
    seen: &Seen,
    resolved: &Path,
    args: &EditArgs,
    applied: &rig_tools::edit::Applied,
) -> String {
    seen.saw_whole(resolved, &applied.text);
    let places = places(&applied.change);
    let at = changed_at(&places, &applied.change, applied.replaced, args.edits.len());
    let mut out = format!("Edited {}: {at}.", args.path);
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
