//! [`Listing`]: which files [`Sandbox::list`](super::Sandbox::list) lists,
//! and the ignore rules it applies the same way for every backend. The
//! rules are read through the sandbox and matched on path text alone, so
//! nothing here touches the host.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::{Override, OverrideBuilder};

use super::{SandboxError, SandboxPath};
use crate::Result;

const IGNORE: &str = ".ignore";
const GITIGNORE: &str = ".gitignore";
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
/// among them, those in `root`'s parents down to `work_dir`, and the work
/// dir's git excludes.
pub(super) fn rule_files(
    all: &[SandboxPath],
    work_dir: &SandboxPath,
    root: &SandboxPath,
) -> Vec<SandboxPath> {
    let mut files: Vec<SandboxPath> = all
        .iter()
        .filter(|path| matches!(name(path), Some(IGNORE | GITIGNORE)))
        .cloned()
        .collect();
    let above = Path::new(root.as_str()).ancestors().skip(1);
    for dir in above.take_while(|dir| dir.starts_with(work_dir.as_str())) {
        let dir = work_dir.join(&dir.to_string_lossy());
        files.extend([dir.join(IGNORE), dir.join(GITIGNORE)]);
    }
    files.push(work_dir.join(EXCLUDE));
    files
}

/// The ignore rules of a work dir: each directory's `.ignore`s, then
/// `.gitignore`s, then the work dir's git excludes, by the directory.
pub(super) struct Rules([HashMap<PathBuf, Gitignore>; 3]);

impl Rules {
    /// The rules in `files`, whose content is `texts` (`None` when missing).
    pub(super) fn new(
        files: &[SandboxPath],
        texts: Vec<Option<Vec<u8>>>,
        work_dir: &SandboxPath,
    ) -> Self {
        let mut rules = Self(Default::default());
        let exclude = work_dir.join(EXCLUDE);
        for (file, text) in files.iter().zip(texts) {
            let Some(text) = text else { continue };
            let (rank, dir) = match name(file) {
                _ if *file == exclude => (2, work_dir.as_str()),
                Some(IGNORE) => (0, parent(file)),
                _ => (1, parent(file)),
            };
            let mut builder = GitignoreBuilder::new(dir);
            // A line that is not a glob is skipped, as git skips it; a
            // leading byte order mark is dropped, as `ignore` drops it.
            let text = String::from_utf8_lossy(&text);
            for line in text.strip_prefix('\u{feff}').unwrap_or(&text).lines() {
                builder
                    .add_line(Some(PathBuf::from(file.as_str())), line)
                    .ok();
            }
            if let Ok(matcher) = builder.build() {
                rules.0[rank].insert(PathBuf::from(dir), matcher);
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
        let (path, root) = (Path::new(path.as_str()), Path::new(root.as_str()));
        let Some(rest) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
            return false;
        };
        listing.keeps(rest)
            && glob.is_none_or(|glob| glob.matched(path, false).is_whitelist())
            && !path
                .ancestors()
                .skip(1)
                .take_while(|dir| *dir != root)
                .any(|dir| self.ignored(dir, true))
            && !self.ignored(path, false)
    }

    /// Whether the rules leave `path` out: of the first rank with a match,
    /// the deepest directory's rules decide.
    fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.0
            .iter()
            .find_map(|rules| {
                path.ancestors().skip(1).find_map(|dir| {
                    match rules.get(dir)?.matched(path, is_dir) {
                        Match::None => None,
                        matched => Some(matched.is_ignore()),
                    }
                })
            })
            .unwrap_or(false)
    }
}

/// The directory `path` is in.
fn parent(path: &SandboxPath) -> &str {
    let parent = Path::new(path.as_str()).parent();
    parent.and_then(Path::to_str).unwrap_or("/")
}

/// The last part of `path`.
fn name(path: &SandboxPath) -> Option<&str> {
    Path::new(path.as_str()).file_name().and_then(OsStr::to_str)
}
