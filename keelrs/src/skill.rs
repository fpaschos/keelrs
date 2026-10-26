//! Skills an agent mounts with [`AgentContext::use_skill`](crate::AgentContext::use_skill).
//!
//! SAMPLE for the open `use_skill` question: a skill is resolved before the agent
//! function runs (embedded at compile time by [`skill!`](crate::skill!)), and the
//! function only declares it, as with Flue's `SKILL.md` imports.

/// A skill embedded into the binary at compile time.
///
/// STUB: only `SKILL.md` is embedded and its frontmatter is not validated;
/// supporting files and validation come with the real design.
#[derive(Debug, Clone, Copy)]
pub struct Skill {
    source: &'static str,
}

impl Skill {
    #[doc(hidden)]
    #[must_use]
    pub const fn embedded(source: &'static str) -> Self {
        Self { source }
    }

    /// `name` from the frontmatter.
    #[must_use]
    pub fn name(&self) -> &str {
        self.field("name").unwrap_or("<unnamed>")
    }

    /// `description` from the frontmatter.
    #[must_use]
    pub fn description(&self) -> &str {
        self.field("description").unwrap_or("")
    }

    fn field(&self, key: &str) -> Option<&str> {
        let frontmatter = self.source.strip_prefix("---\n")?.split("\n---").next()?;
        frontmatter
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':').map(str::trim))
    }
}

/// Embeds the `SKILL.md` at `path` (relative to the calling file) as a [`Skill`].
#[macro_export]
macro_rules! skill {
    ($path:literal) => {
        $crate::Skill::embedded(include_str!($path))
    };
}
