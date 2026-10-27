//! Skill discovery: skill folders found on disk at runtime, leniently.
//!
//! Each root holds skill folders one level deep (`<root>/<name>/SKILL.md`). A folder
//! that cannot be a skill is skipped with a [`SkillProblem`]; discovery never fails.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::fs;

use super::frontmatter::{self, Frontmatter};
use super::{Files, Parsed, Skill, Source, clutter, secret};

/// Where [`discover`] looks for skill folders: roots in order, the first found wins.
#[derive(Debug, Clone, Default)]
pub struct Discover {
    roots: Vec<PathBuf>,
}

impl Discover {
    /// No roots yet; add them with [`root`](Self::root).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The conventional roots, project before user: `<project>/.agents/skills`,
    /// `<project>/.claude/skills`, `~/.agents/skills`, `~/.claude/skills`. The
    /// user roots are left out when there is no home directory.
    #[must_use]
    pub fn conventional(project: impl Into<PathBuf>) -> Self {
        let project = project.into();
        let mut discover = Self::new()
            .root(project.join(".agents/skills"))
            .root(project.join(".claude/skills"));
        if let Some(home) = std::env::home_dir() {
            discover = discover
                .root(home.join(".agents/skills"))
                .root(home.join(".claude/skills"));
        }
        discover
    }

    /// Appends `path` as the next root.
    #[must_use]
    pub fn root(mut self, path: impl Into<PathBuf>) -> Self {
        self.roots.push(path.into());
        self
    }
}

/// What [`discover`] found: the skills, and the folders it skipped and why.
#[derive(Debug, Clone, Default)]
pub struct Discovered {
    /// The valid skills, in discovery order, names unique.
    pub skills: Vec<Skill>,
    /// What was skipped, in discovery order.
    pub problems: Vec<SkillProblem>,
}

impl Discovered {
    /// The discovered skill named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name() == name)
    }
}

/// The skills, so `cx.use_skills(&discovered)` mounts them all.
impl<'a> IntoIterator for &'a Discovered {
    type Item = &'a Skill;
    type IntoIter = std::slice::Iter<'a, Skill>;

    fn into_iter(self) -> Self::IntoIter {
        self.skills.iter()
    }
}

/// A skill folder, or a file in one, that discovery skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillProblem {
    /// The skipped folder or file.
    pub path: PathBuf,
    /// Why it was skipped.
    pub reason: String,
}

/// Finds the skill folders under `discover`'s roots, in root order, then by
/// folder name.
pub async fn discover(discover: Discover) -> Discovered {
    let mut discovered = Discovered::default();
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for root in &discover.roots {
        let Ok(mut entries) = fs::read_dir(root).await else {
            continue;
        };
        let mut folders = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            folders.push(entry.path());
        }
        folders.sort();
        for folder in folders {
            let (frontmatter, instructions) = match parse(&folder).await {
                Ok(Some(parsed)) => parsed,
                Ok(None) => continue,
                Err(reason) => {
                    discovered.problems.push(SkillProblem {
                        path: folder,
                        reason,
                    });
                    continue;
                }
            };
            if let Some(first) = seen.get(&frontmatter.name) {
                let reason = format!(
                    "skill `{}` is already discovered at `{}`",
                    frontmatter.name,
                    first.display()
                );
                discovered.problems.push(SkillProblem {
                    path: folder,
                    reason,
                });
                continue;
            }
            let (paths, problems) = files(&folder).await;
            seen.insert(frontmatter.name.clone(), folder.clone());
            discovered.skills.push(Skill(Source::Owned(Arc::new(Parsed {
                frontmatter,
                instructions,
                files: Files::Disk { folder, paths },
            }))));
            discovered.problems.extend(problems);
        }
    }
    discovered
}

/// The frontmatter and body of `folder`'s `SKILL.md`, `None` without one, or why
/// it is not a valid one.
async fn parse(folder: &Path) -> Result<Option<(Frontmatter, String)>, String> {
    let path = folder.join("SKILL.md");
    if !fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let source = fs::read_to_string(&path)
        .await
        .map_err(|e| format!("`SKILL.md` cannot be read: {e}"))?;
    let (frontmatter, body) = frontmatter::parse(&source)?;
    Ok(Some((frontmatter, body.to_owned())))
}

/// The supporting files under `folder` by path relative to it, `/`-separated
/// and sorted, `SKILL.md` aside, and what was left out: secrets and symlinks
/// that leave the folder. Clutter is skipped unread; a symlinked folder is
/// walked once.
async fn files(folder: &Path) -> (Vec<String>, Vec<SkillProblem>) {
    let (mut paths, mut problems) = (Vec::new(), Vec::new());
    let Ok(root) = fs::canonicalize(folder).await else {
        return (paths, problems);
    };
    let mut followed = HashSet::from([root.clone()]);
    let mut pending = vec![String::new()];
    while let Some(dir) = pending.pop() {
        let Ok(mut entries) = fs::read_dir(folder.join(&dir)).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if clutter(&name) {
                continue;
            }
            let path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            if secret(&name) {
                problems.push(SkillProblem {
                    path: folder.join(&path),
                    reason: "may hold a secret; left out of the skill's files".to_owned(),
                });
                continue;
            }
            let Ok(mut kind) = entry.file_type().await else {
                continue;
            };
            if kind.is_symlink() {
                let target = fs::canonicalize(entry.path()).await.ok();
                let Some(target) = target.filter(|target| target.starts_with(&root)) else {
                    problems.push(SkillProblem {
                        path: folder.join(&path),
                        reason: "a symlink that leaves the skill's folder; left out of the skill's files"
                            .to_owned(),
                    });
                    continue;
                };
                let Ok(metadata) = fs::metadata(&target).await else {
                    continue;
                };
                if metadata.is_dir() && !followed.insert(target) {
                    continue;
                }
                kind = metadata.file_type();
            }
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && path != "SKILL.md" {
                paths.push(path);
            }
        }
    }
    paths.sort();
    problems.sort_by(|a, b| a.path.cmp(&b.path));
    (paths, problems)
}

/// The bytes of the file `path` in a discovered skill's `folder`, refusing a
/// symlink that leaves the folder.
pub(super) async fn read(folder: &Path, path: &str) -> Result<Vec<u8>, String> {
    let unreadable = |e: std::io::Error| format!("`{path}` cannot be read: {e}");
    let folder = fs::canonicalize(folder).await.map_err(unreadable)?;
    let file = fs::canonicalize(folder.join(path))
        .await
        .map_err(unreadable)?;
    if !file.starts_with(&folder) {
        return Err(format!("`{path}` leaves the skill's folder"));
    }
    fs::read(file).await.map_err(unreadable)
}
