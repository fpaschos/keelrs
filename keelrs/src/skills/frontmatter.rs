//! A `SKILL.md`'s frontmatter: only `name` and `description` are read; other
//! fields (`license`, `model`, `argument-hint`, …) are ignored.

use serde::Deserialize;

/// The two fields keelrs reads, both required and non-empty.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct Frontmatter {
    pub(super) name: String,
    pub(super) description: String,
}

impl Frontmatter {
    /// Why this frontmatter cannot name a skill, if it cannot.
    pub(super) fn check(&self) -> Result<(), String> {
        for (field, value) in [("name", &self.name), ("description", &self.description)] {
            if value.trim().is_empty() {
                return Err(format!("`{field}` must not be empty"));
            }
        }
        Ok(())
    }
}

/// Splits a `SKILL.md` into its frontmatter and its body, trimmed. On a YAML
/// error it retries once with unquoted values that hold `": "` quoted, as Claude
/// Code skills often write `description: Use when: …`.
pub(super) fn parse(source: &str) -> Result<(Frontmatter, &str), String> {
    let (yaml, body) = split(source).ok_or_else(|| {
        "`SKILL.md` must start with YAML frontmatter between `---` lines".to_owned()
    })?;
    let frontmatter: Frontmatter = match serde_saphyr::from_str(yaml) {
        Ok(frontmatter) => frontmatter,
        Err(error) => serde_saphyr::from_str(&quote_colons(yaml))
            .map_err(|_| format!("frontmatter: {error}"))?,
    };
    frontmatter.check()?;
    Ok((frontmatter, body.trim()))
}

/// `yaml` with each top-level `key: value` whose plain value holds `": "` rewritten
/// as `key: "value"`.
fn quote_colons(yaml: &str) -> String {
    yaml.lines()
        .map(|line| match line.split_once(": ") {
            Some((key, value))
                if !line.starts_with([' ', '\t', '#'])
                    && value.contains(": ")
                    && !value.starts_with(['"', '\'', '|', '>', '[', '{']) =>
            {
                let value = value.trim_end().replace('\\', "\\\\").replace('"', "\\\"");
                format!("{key}: \"{value}\"\n")
            }
            _ => format!("{line}\n"),
        })
        .collect()
}

/// The YAML between the opening and closing `---` lines, and what follows.
fn split(source: &str) -> Option<(&str, &str)> {
    let rest = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}
