//! Skills an agent mounts with [`AgentContext::use_skill`](crate::AgentContext::use_skill).
//!
//! A [`Skill`] is a `SKILL.md` (Agent Skills spec) plus supporting files, resolved
//! before the agent function runs: embedded at compile time with [`skill!`](crate::skill!)
//! built in code with [`Skill::new`], or found on disk with [`discover`]. The model sees the catalog line and reads
//! the rest through the built-in `activate_skill` and `read_skill_resource` tools.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use frontmatter::Frontmatter;
use include_dir::{Dir, DirEntry};

pub(crate) mod catalog;
mod discover;
mod frontmatter;
pub(crate) mod tools;

pub use discover::{Discover, Discovered, SkillProblem, discover};

/// A skill an agent mounts with [`AgentContext::use_skill`](crate::AgentContext::use_skill).
///
/// Cheap to clone: it holds an embedded skill by `&'static`, a built or
/// discovered one by `Arc`.
#[derive(Clone)]
pub struct Skill(Source);

#[derive(Clone)]
enum Source {
    Embedded(&'static Embedded),
    Owned(Arc<Parsed>),
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
            files: Files::Memory(files),
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
    secret(name) || clutter(name)
}

/// A file or folder name that may hold a secret. Templates (`.env.example`,
/// `.env.sample`, `.env.template`) and Markdown about secrets do not.
fn secret(name: &str) -> bool {
    let template = [".example", ".sample", ".template"]
        .iter()
        .any(|suffix| name.ends_with(suffix));
    name == ".ssh"
        || (name.starts_with("secrets") && !name.ends_with(".md"))
        || (name.starts_with(".env") && !template)
        || name.ends_with(".pem")
        || name.ends_with(".key")
}

/// A build or VCS folder.
fn clutter(name: &str) -> bool {
    matches!(name, ".git" | "node_modules" | "dist" | "target")
}

/// A valid skill: its frontmatter, `SKILL.md` body and other files.
#[derive(Debug, Clone)]
struct Parsed {
    frontmatter: Frontmatter,
    instructions: String,
    files: Files,
}

/// A skill's supporting files by path relative to its folder.
#[derive(Debug, Clone)]
enum Files {
    /// Embedded or built in code: the bytes.
    Memory(BTreeMap<String, Vec<u8>>),
    /// Discovered: the paths found in `folder`, read on request.
    Disk { folder: PathBuf, paths: Vec<String> },
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
        Ok(Self(Source::Owned(Arc::new(Parsed {
            frontmatter,
            instructions: instructions.into(),
            files: Files::Memory(BTreeMap::new()),
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
        let mut parsed = match self.0 {
            Source::Owned(parsed) if matches!(parsed.files, Files::Memory(_)) => parsed,
            _ => {
                return Err(SkillError::new(
                    self.name(),
                    "an embedded or discovered skill's files come from its folder".to_owned(),
                ));
            }
        };
        let path =
            normalize(&path).map_err(|reason| SkillError::new(&parsed.frontmatter.name, reason))?;
        if let Files::Memory(files) = &mut Arc::make_mut(&mut parsed).files {
            files.insert(path, bytes.into());
        }
        Ok(Self(Source::Owned(parsed)))
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

    /// The supporting files' paths relative to the skill's folder, sorted.
    pub(crate) fn files(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        match self.valid().map(|p| &p.files) {
            Some(Files::Memory(files)) => Box::new(files.keys().map(String::as_str)),
            Some(Files::Disk { paths, .. }) => Box::new(paths.iter().map(String::as_str)),
            None => Box::new(std::iter::empty()),
        }
    }

    /// The bytes of the supporting file `path`, one of [`files`](Self::files);
    /// `None` when it is not one. A discovered skill's file is read from disk now.
    pub(crate) async fn read(&self, path: &str) -> Option<Result<Cow<'_, [u8]>, String>> {
        match &self.valid()?.files {
            Files::Memory(files) => files.get(path).map(|bytes| Ok(Cow::from(bytes.as_slice()))),
            Files::Disk { folder, paths } => {
                if !paths.iter().any(|file| file == path) {
                    return None;
                }
                Some(discover::read(folder, path).await.map(Cow::from))
            }
        }
    }

    fn parsed(&self) -> Result<&Parsed, &SkillError> {
        match &self.0 {
            Source::Embedded(embedded) => embedded.parsed().as_ref(),
            Source::Owned(parsed) => Ok(parsed),
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

/// A supporting file's path, which must stay inside the skill's folder, in the
/// form files are listed by: `/`-separated, without `.` or empty segments.
fn normalize(path: &str) -> Result<String, String> {
    let refusal =
        || format!("file path `{path}` must be relative to the skill's folder, without `..`");
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => segments.push(segment.to_string_lossy()),
            Component::CurDir => {}
            _ => return Err(refusal()),
        }
    }
    if segments.is_empty() {
        return Err(refusal());
    }
    Ok(segments.join("/"))
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
/// (`.git`, `.ssh`, `node_modules`, `dist`, `target`, `secrets*` but Markdown,
/// `.env*` but `.example`, `.sample` and `.template`, `*.pem`, `*.key`). Symlinks
/// are followed at compile time.
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
