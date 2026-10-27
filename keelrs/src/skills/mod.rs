//! Skills an agent mounts with [`AgentContext::use_skill`](crate::AgentContext::use_skill).
//!
//! A [`Skill`] is a `SKILL.md` (Agent Skills spec) plus supporting files, resolved
//! before the agent function runs: embedded at compile time with [`skill!`](crate::skill!)
//! or built in code with [`Skill::new`]. The model sees the catalog line and reads
//! the rest through the built-in `activate_skill` and `read_skill_resource` tools.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path};
use std::sync::{Arc, OnceLock};

use frontmatter::Frontmatter;
use include_dir::{Dir, DirEntry};

pub(crate) mod catalog;
mod frontmatter;
pub(crate) mod tools;

/// A skill an agent mounts with [`AgentContext::use_skill`](crate::AgentContext::use_skill).
///
/// Cheap to clone: it holds an embedded skill by `&'static` or a built one by `Arc`.
#[derive(Clone)]
pub struct Skill(Source);

#[derive(Clone)]
enum Source {
    Embedded(&'static Embedded),
    Built(Arc<Parsed>),
}

/// A skill folder embedded by [`skill!`](crate::skill!), parsed on first use.
#[doc(hidden)]
#[derive(Debug)]
pub struct Embedded {
    path: &'static str,
    dir: &'static Dir<'static>,
    parsed: OnceLock<Result<Parsed, SkillError>>,
}

impl Embedded {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(path: &'static str, dir: &'static Dir<'static>) -> Self {
        Self {
            path,
            dir,
            parsed: OnceLock::new(),
        }
    }

    fn parsed(&self) -> &Result<Parsed, SkillError> {
        self.parsed.get_or_init(|| {
            let folder = self.path.rsplit('/').next().unwrap_or(self.path);
            self.parse()
                .map_err(|reason| SkillError::new(folder, reason))
        })
    }

    fn parse(&self) -> Result<Parsed, String> {
        let mut files = BTreeMap::new();
        collect(self.dir, &mut files)?;
        let source = files
            .remove("SKILL.md")
            .ok_or_else(|| "the folder has no `SKILL.md`".to_owned())?;
        let source = String::from_utf8(source).map_err(|_| "`SKILL.md` is not UTF-8".to_owned())?;
        let (frontmatter, body) = frontmatter::parse(&source)?;
        Ok(Parsed {
            frontmatter,
            instructions: body.to_owned(),
            files,
        })
    }
}

/// Every file under `dir` by its path from the skill's folder, refusing files
/// that must not ship inside a binary.
fn collect(dir: &Dir<'_>, files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    for entry in dir.entries() {
        let path = entry.path().to_string_lossy();
        if path.split('/').any(forbidden) {
            return Err(format!(
                "`{path}` would be embedded into the binary; remove it from the skill's folder"
            ));
        }
        match entry {
            DirEntry::Dir(dir) => collect(dir, files)?,
            DirEntry::File(file) => {
                files.insert(path.into_owned(), file.contents().to_vec());
            }
        }
    }
    Ok(())
}

/// Secrets and build or VCS clutter, after Flue's packaging rules.
fn forbidden(name: &str) -> bool {
    matches!(name, ".git" | ".ssh" | "node_modules" | "dist" | "target")
        || name.starts_with("secrets")
        || name.starts_with(".env")
        || name.ends_with(".pem")
        || name.ends_with(".key")
}

/// A valid skill: its frontmatter, `SKILL.md` body and other files by relative path.
#[derive(Debug, Clone)]
struct Parsed {
    frontmatter: Frontmatter,
    instructions: String,
    files: BTreeMap<String, Vec<u8>>,
}

/// Why a skill is invalid: an empty `name` or `description`, a bad file path or
/// an embedded folder that cannot be a skill.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("skill `{skill}`: {reason}")]
pub struct SkillError {
    skill: String,
    reason: String,
}

impl SkillError {
    fn new(skill: &str, reason: String) -> Self {
        Self {
            skill: skill.to_owned(),
            reason,
        }
    }
}

impl Skill {
    #[doc(hidden)]
    #[must_use]
    pub const fn embedded(embedded: &'static Embedded) -> Self {
        Self(Source::Embedded(embedded))
    }

    /// A skill built in code, like Flue's `defineSkill`: `instructions` is the
    /// `SKILL.md` body.
    ///
    /// # Errors
    ///
    /// When `name` or `description` is empty.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<Self, SkillError> {
        let frontmatter = Frontmatter {
            name: name.into(),
            description: description.into(),
        };
        frontmatter
            .check()
            .map_err(|reason| SkillError::new(&frontmatter.name, reason))?;
        Ok(Self(Source::Built(Arc::new(Parsed {
            frontmatter,
            instructions: instructions.into(),
            files: BTreeMap::new(),
        }))))
    }

    /// Adds a supporting file at `path`, relative to the skill's folder.
    ///
    /// # Errors
    ///
    /// When `path` is empty, absolute or contains `..`.
    pub fn file(
        self,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, SkillError> {
        let path = path.into();
        let Source::Built(mut parsed) = self.0 else {
            return Err(SkillError::new(
                self.name(),
                "an embedded skill's files come from its folder".to_owned(),
            ));
        };
        check_path(&path).map_err(|reason| SkillError::new(&parsed.frontmatter.name, reason))?;
        Arc::make_mut(&mut parsed).files.insert(path, bytes.into());
        Ok(Self(Source::Built(parsed)))
    }

    /// Why the skill cannot be mounted, if it cannot.
    pub(crate) fn validate(&self) -> Result<(), SkillError> {
        self.parsed().map(|_| ()).map_err(SkillError::clone)
    }

    /// `name` from the frontmatter; an invalid embedded skill's folder name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self.parsed() {
            Ok(parsed) => &parsed.frontmatter.name,
            Err(error) => &error.skill,
        }
    }

    /// `description` from the frontmatter; empty for an invalid embedded skill.
    #[must_use]
    pub fn description(&self) -> &str {
        self.valid().map_or("", |p| &p.frontmatter.description)
    }

    /// The `SKILL.md` body, frontmatter stripped.
    pub(crate) fn instructions(&self) -> &str {
        self.valid().map_or("", |p| &p.instructions)
    }

    /// The supporting files by relative path, sorted.
    pub(crate) fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.valid().into_iter().flat_map(|p| {
            p.files
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        })
    }

    fn parsed(&self) -> Result<&Parsed, &SkillError> {
        match &self.0 {
            Source::Embedded(embedded) => embedded.parsed().as_ref(),
            Source::Built(parsed) => Ok(parsed),
        }
    }

    fn valid(&self) -> Option<&Parsed> {
        self.parsed().ok()
    }
}

impl From<&Self> for Skill {
    fn from(skill: &Self) -> Self {
        skill.clone()
    }
}

impl fmt::Debug for Skill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Skill")
            .field("name", &self.name())
            .finish_non_exhaustive()
    }
}

/// A supporting file's path must stay inside the skill's folder.
fn check_path(path: &str) -> Result<(), String> {
    let relative = Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if path.is_empty() || !relative {
        return Err(format!(
            "file path `{path}` must be relative to the skill's folder, without `..`"
        ));
    }
    Ok(())
}

/// Whether a [`skill!`](crate::skill!) path starts at the crate root.
#[doc(hidden)]
#[must_use]
pub const fn from_manifest_dir(path: &str) -> bool {
    let (path, prefix) = (path.as_bytes(), b"$CARGO_MANIFEST_DIR/");
    if path.len() < prefix.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if path[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Embeds the whole skill folder at `path` as a [`Skill`]: its `SKILL.md` and
/// every other file in it, subfolders included.
///
/// `path` starts at the crate root, as `"$CARGO_MANIFEST_DIR/skills/review"`.
/// A missing folder fails to compile. The rest is checked when an agent first
/// mounts the skill, and a broken skill settles that dispatch as
/// [`Outcome::Failed`](crate::Outcome::Failed): a missing `SKILL.md`, an empty
/// `name` or `description`, or a file that must not ship inside a binary
/// (`.git`, `.ssh`, `node_modules`, `dist`, `target`, `secrets*`, `.env*`,
/// `*.pem`, `*.key`). Symlinks are followed at compile time.
///
/// ```ignore
/// static REVIEW: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/skills/review");
/// ```
#[macro_export]
macro_rules! skill {
    // `tt`, not `literal`: `include_dir!` rejects a literal wrapped in a macro group.
    ($path:tt) => {{
        use $crate::__private::include_dir;
        const _: () = assert!(
            $crate::skills::from_manifest_dir($path),
            "`skill!` paths start at the crate root: \"$CARGO_MANIFEST_DIR/<folder>\""
        );
        static DIR: include_dir::Dir<'static> = include_dir::include_dir!($path);
        static EMBEDDED: $crate::skills::Embedded = $crate::skills::Embedded::new($path, &DIR);
        $crate::Skill::embedded(&EMBEDDED)
    }};
}
