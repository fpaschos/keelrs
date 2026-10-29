//! [`Listing`]: which files [`Sandbox::list`](super::Sandbox::list) lists,
//! and the ignore rules it applies the same way for every backend. The
//! rules are read through the sandbox and matched on path text alone, so
//! nothing here touches the host.

use std::collections::HashMap;
use std::path::PathBuf;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::{Override, OverrideBuilder};

use super::{SandboxError, SandboxPath};
use crate::Result;

/// A directory's ignore file that wins over its `.gitignore`.
const IGNORE: &str = ".ignore";

/// A directory's git ignore file.
const GITIGNORE: &str = ".gitignore";

/// The work dir's own git excludes, below its `.gitignore`s.
const EXCLUDE: &str = ".git/info/exclude";

/// Which files [`Sandbox::list`](super::Sandbox::list) lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Listing {
    /// What a person browsing the project sees: no file the ignore rules
    /// (`.gitignore`, `.ignore`, `.git/info/exclude`) leave out, no hidden
    /// file or directory, no `target` directory.
    #[default]
    Visible,
    /// Every file the ignore rules keep, hidden ones included; `.git` left out.
    Hidden,
    /// Every regular file.
    All,
}

impl Listing {
    /// `Hidden` for `Visible` when `glob` names a hidden file or directory,
    /// such as `.github/**`, so asking for one finds it.
    pub(super) fn for_glob(self, glob: Option<&str>) -> Self {
        let names_hidden = glob.is_some_and(|glob| {
            glob.split('/')
                .any(|part| part.starts_with('.') && part != "." && part != "..")
        });
        match self {
            Self::Visible if names_hidden => Self::Hidden,
            other => other,
        }
    }

    /// Whether it keeps `below`, a path below the listed root, by its names alone.
    fn keeps(self, below: &str) -> bool {
        let mut parts = below.split('/');
        match self {
            Self::Visible => !parts.any(|part| part == "target" || part.starts_with('.')),
            Self::Hidden => !parts.any(|part| part == ".git"),
            Self::All => true,
        }
    }
}

/// `glob` as a matcher of paths under `root`.
pub(super) fn glob(root: &SandboxPath, glob: &str) -> Result<Override> {
    OverrideBuilder::new(root.as_str())
        .add(glob)
        .and_then(|builder| builder.build())
        .map_err(|e| {
            SandboxError::Io {
                operation: "list",
                path: root.to_string(),
                kind: std::io::ErrorKind::InvalidInput,
                message: format!("bad glob: {e}"),
            }
            .into()
        })
}

/// The rule files that may apply to `all`, the files under `root`: those
/// among them, those in `root` and its parents down to `work_dir`, and the
/// work dir's git excludes.
pub(super) fn rule_files(
    all: &[SandboxPath],
    work_dir: &SandboxPath,
    root: &SandboxPath,
) -> Vec<SandboxPath> {
    let mut files: Vec<SandboxPath> = all
        .iter()
        .filter(|path| matches!(name(path.as_str()), IGNORE | GITIGNORE))
        .cloned()
        .collect();
    for dir in ancestors(root.as_str()).filter(|dir| below(dir, work_dir.as_str()).is_some()) {
        let dir = work_dir.join(dir);
        files.extend([dir.join(IGNORE), dir.join(GITIGNORE)]);
    }
    files.push(work_dir.join(EXCLUDE));
    files.sort();
    files.dedup();
    files
}

/// The ignore rules of a work dir, from its rule files.
pub(super) struct Rules {
    /// Each directory's `.ignore`, by the directory.
    ignore: HashMap<String, Gitignore>,
    /// Each directory's `.gitignore`, by the directory.
    gitignore: HashMap<String, Gitignore>,
    /// The work dir's git excludes.
    exclude: Option<Gitignore>,
}

impl Rules {
    /// The rules in `files`, whose content is `texts` (`None` when missing).
    pub(super) fn new(
        files: &[SandboxPath],
        texts: Vec<Option<Vec<u8>>>,
        work_dir: &SandboxPath,
    ) -> Self {
        let mut rules = Self {
            ignore: HashMap::new(),
            gitignore: HashMap::new(),
            exclude: None,
        };
        let exclude = work_dir.join(EXCLUDE);
        for (file, text) in files.iter().zip(texts) {
            let Some(text) = text else { continue };
            let file = file.as_str();
            let dir = if file == exclude.as_str() {
                work_dir.as_str()
            } else {
                parent(file)
            };
            let mut builder = GitignoreBuilder::new(dir);
            // A line that is not a glob is skipped, as git skips it.
            for line in String::from_utf8_lossy(&text).lines() {
                builder.add_line(Some(PathBuf::from(file)), line).ok();
            }
            let Ok(matcher) = builder.build() else {
                continue;
            };
            match name(file) {
                _ if file == exclude.as_str() => rules.exclude = Some(matcher),
                IGNORE => _ = rules.ignore.insert(dir.to_owned(), matcher),
                _ => _ = rules.gitignore.insert(dir.to_owned(), matcher),
            }
        }
        rules
    }

    /// Whether `listing` of `root` keeps `path`, which is below it: by its
    /// names, `glob`, and no directory on the way nor the file itself
    /// ignored.
    pub(super) fn keeps(
        &self,
        path: &SandboxPath,
        root: &SandboxPath,
        listing: Listing,
        glob: Option<&Override>,
    ) -> bool {
        let path = path.as_str();
        let Some(rest) = below(path, root.as_str()) else {
            return false;
        };
        let start = path.len() - rest.len();
        listing.keeps(rest)
            && glob.is_none_or(|glob| glob.matched(path, false).is_whitelist())
            && !rest
                .match_indices('/')
                .any(|(at, _)| self.ignored(&path[..start + at], true))
            && !self.ignored(path, false)
    }

    /// Whether the rules leave `path` out: the deepest `.ignore` that
    /// matches decides, else the deepest `.gitignore`, else the excludes.
    fn ignored(&self, path: &str, is_dir: bool) -> bool {
        let decide = |matcher: &Gitignore| match matcher.matched(path, is_dir) {
            Match::None => None,
            matched => Some(matched.is_ignore()),
        };
        let deepest = |rules: &HashMap<String, Gitignore>| {
            ancestors(path)
                .skip(1)
                .find_map(|dir| rules.get(dir).and_then(decide))
        };
        deepest(&self.ignore)
            .or_else(|| deepest(&self.gitignore))
            .or_else(|| self.exclude.as_ref().and_then(decide))
            .unwrap_or(false)
    }
}

/// `path`, then each of its parents up to `/`.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |path| (*path != "/").then(|| parent(path)))
}

/// The directory `path` is in; `/` for `/`.
fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &path[..at],
    }
}

/// The last part of `path`.
fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What of `path` lies below `dir`: empty when they are one, `None` when
/// `path` is not in it.
fn below<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    if dir == "/" {
        return path.strip_prefix('/');
    }
    match path.strip_prefix(dir)? {
        "" => Some(""),
        rest => rest.strip_prefix('/'),
    }
}
