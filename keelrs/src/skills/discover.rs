//! Skill discovery: skill folders found on disk at runtime, leniently.
//!
//! Each root holds skill folders one level deep (`<root>/<name>/SKILL.md`). A folder
//! that cannot be a skill is skipped with a [`SkillProblem`]; discovery never fails.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::fs;

use super::{Files, Parsed, Skill, Source, clutter, frontmatter, secret};

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
            let (skill, problems) = match load(&folder).await {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(reason) => {
                    discovered.problems.push(SkillProblem {
                        path: folder,
                        reason,
                    });
                    continue;
                }
            };
            if let Some(first) = seen.get(skill.name()) {
                let reason = format!(
                    "skill `{}` is already discovered at `{}`",
                    skill.name(),
                    first.display()
                );
                discovered.problems.push(SkillProblem {
                    path: folder,
                    reason,
                });
                continue;
            }
            seen.insert(skill.name().to_owned(), folder);
            discovered.skills.push(skill);
            discovered.problems.extend(problems);
        }
    }
    discovered
}

/// The skill in `folder` and the files it left out, `None` without a `SKILL.md`,
/// or why it is not a valid one.
async fn load(folder: &Path) -> Result<Option<(Skill, Vec<SkillProblem>)>, String> {
    let path = folder.join("SKILL.md");
    if !fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let source = fs::read_to_string(&path)
        .await
        .map_err(|e| format!("`SKILL.md` cannot be read: {e}"))?;
    let (frontmatter, body) = frontmatter::parse_lenient(&source)?;
    let (paths, problems) = files(folder).await;
    let skill = Skill(Source::Owned(Arc::new(Parsed {
        frontmatter,
        instructions: body.to_owned(),
        files: Files::Disk {
            folder: folder.to_owned(),
            paths,
        },
    })));
    Ok(Some((skill, problems)))
}

/// The supporting files under `folder` by path relative to it, `/`-separated
/// and sorted, `SKILL.md` aside, and the secrets left out. Clutter is skipped
/// unread.
async fn files(folder: &Path) -> (Vec<String>, Vec<SkillProblem>) {
    let (mut paths, mut problems) = (Vec::new(), Vec::new());
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
            match entry.file_type().await {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() && path != "SKILL.md" => paths.push(path),
                _ => {}
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
