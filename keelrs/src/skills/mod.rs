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

/// A skill embedded by [`skill!`](crate::skill!), parsed on first use.
#[doc(hidden)]
#[derive(Debug)]
pub struct Embedded {
    dir: &'static str,
    source: &'static str,
    files: &'static [(&'static str, &'static [u8])],
    parsed: OnceLock<Result<Parsed, SkillError>>,
}

impl Embedded {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(
        dir: &'static str,
        source: &'static str,
        files: &'static [(&'static str, &'static [u8])],
    ) -> Self {
        Self {
            dir,
            source,
            files,
            parsed: OnceLock::new(),
        }
    }

    fn parsed(&self) -> &Result<Parsed, SkillError> {
        self.parsed.get_or_init(|| {
            let folder = folder_name(self.dir);
            let (frontmatter, body) = frontmatter::parse(self.source)
                .map_err(|reason| SkillError::new(folder, reason))?;
            frontmatter
                .check(Some(folder))
                .map_err(|reason| SkillError::new(folder, reason))?;
            let mut files = BTreeMap::new();
            for (path, bytes) in self.files {
                check_path(path).map_err(|reason| SkillError::new(folder, reason))?;
                files.insert((*path).to_owned(), bytes.to_vec());
            }
            Ok(Parsed {
                frontmatter,
                instructions: body.to_owned(),
                files,
            })
        })
    }
}

/// A valid skill: its frontmatter, `SKILL.md` body and other files by relative path.
#[derive(Debug, Clone)]
struct Parsed {
    frontmatter: Frontmatter,
    instructions: String,
    files: BTreeMap<String, Vec<u8>>,
}

/// Why a skill is invalid: a frontmatter rule it breaks, or a bad file path.
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
    /// `SKILL.md` body. `name` and `description` follow the Agent Skills spec.
    ///
    /// # Errors
    ///
    /// When `name` or `description` breaks the spec.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<Self, SkillError> {
        let frontmatter = Frontmatter {
            name: name.into(),
            description: description.into(),
            ..Frontmatter::default()
        };
        frontmatter
            .check(None)
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
    /// When `path` is empty, absolute, contains `..` or is `SKILL.md`.
    pub fn file(
        self,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, SkillError> {
        let path = path.into();
        let Source::Built(mut parsed) = self.0 else {
            return Err(SkillError::new(
                self.name(),
                "an embedded skill's files are listed in `skill!`".to_owned(),
            ));
        };
        check_path(&path).map_err(|reason| SkillError::new(&parsed.frontmatter.name, reason))?;
        Arc::make_mut(&mut parsed).files.insert(path, bytes.into());
        Ok(Self(Source::Built(parsed)))
    }

    /// Checks the skill against the Agent Skills spec, strictly: an embedded
    /// skill that fails it settles its first dispatch as
    /// [`Outcome::Failed`](crate::Outcome::Failed).
    ///
    /// # Errors
    ///
    /// The first rule the skill breaks.
    pub fn validate(&self) -> Result<(), SkillError> {
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

    /// `license` from the frontmatter.
    #[must_use]
    pub fn license(&self) -> Option<&str> {
        self.valid()?.frontmatter.license.as_deref()
    }

    /// `compatibility` from the frontmatter.
    #[must_use]
    pub fn compatibility(&self) -> Option<&str> {
        self.valid()?.frontmatter.compatibility.as_deref()
    }

    /// `metadata` from the frontmatter.
    #[must_use]
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        static EMPTY: BTreeMap<String, String> = BTreeMap::new();
        self.valid().map_or(&EMPTY, |p| &p.frontmatter.metadata)
    }

    /// `allowed-tools` from the frontmatter, split on whitespace; kept, not enforced.
    pub fn allowed_tools(&self) -> impl Iterator<Item = &str> {
        self.valid()
            .and_then(|p| p.frontmatter.allowed_tools.as_deref())
            .unwrap_or("")
            .split_whitespace()
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

/// The last component of an embedded skill's folder path.
fn folder_name(dir: &str) -> &str {
    dir.trim_end_matches('/').rsplit('/').next().unwrap_or(dir)
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
    if path == "SKILL.md" {
        return Err("`SKILL.md` is the skill's instructions, not a supporting file".to_owned());
    }
    Ok(())
}

/// Embeds the skill in the folder `dir` (relative to the calling file) as a
/// [`Skill`]: its `SKILL.md` and each file listed in `files`, relative to `dir`.
///
/// ```ignore
/// static REVIEW: Skill = keelrs::skill!("../skills/review", files: ["references/style.md"]);
/// ```
///
/// A listed file that does not exist fails to compile; the frontmatter is
/// checked on first use (see [`Skill::validate`]).
#[macro_export]
macro_rules! skill {
    ($dir:literal $(, files: [$($file:literal),* $(,)?])? $(,)?) => {{
        static EMBEDDED: $crate::skills::Embedded = $crate::skills::Embedded::new(
            $dir,
            include_str!(concat!($dir, "/SKILL.md")),
            &[$($(($file, include_bytes!(concat!($dir, "/", $file)))),*)?],
        );
        $crate::Skill::embedded(&EMBEDDED)
    }};
}
