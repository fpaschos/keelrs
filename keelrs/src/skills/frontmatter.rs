//! A `SKILL.md`'s frontmatter: the Agent Skills spec's six fields and their rules.
//!
//! Strict for embedded and code-built skills: any broken rule or extra field is an error.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The spec's six fields, as written in `SKILL.md`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct Frontmatter {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) license: Option<String>,
    pub(super) compatibility: Option<String>,
    #[serde(default)]
    pub(super) metadata: BTreeMap<String, String>,
    pub(super) allowed_tools: Option<String>,
}

impl Frontmatter {
    /// The first spec rule this frontmatter breaks; `folder` is the skill's
    /// folder name, which `name` must match.
    pub(super) fn check(&self, folder: Option<&str>) -> Result<(), String> {
        check_name(&self.name)?;
        if let Some(folder) = folder.filter(|folder| *folder != self.name) {
            return Err(format!(
                "`name` is `{}` but the folder is `{folder}`; they must match",
                self.name
            ));
        }
        check_length("description", &self.description, 1024)?;
        if let Some(compatibility) = &self.compatibility {
            check_length("compatibility", compatibility, 500)?;
        }
        Ok(())
    }
}

/// 1–64 characters of `a-z0-9-`, no leading, trailing or double hyphen.
fn check_name(name: &str) -> Result<(), String> {
    check_length("name", name, 64)?;
    let allowed = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !allowed || name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!(
            "`name` `{name}` must be lowercase letters, digits and single hyphens, \
             not starting or ending with a hyphen"
        ));
    }
    Ok(())
}

fn check_length(field: &str, value: &str, max: usize) -> Result<(), String> {
    let length = value.chars().count();
    if value.trim().is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    if length > max {
        return Err(format!(
            "`{field}` is {length} characters; at most {max} are allowed"
        ));
    }
    Ok(())
}

/// Splits a `SKILL.md` into its frontmatter and its body, trimmed.
pub(super) fn parse(source: &str) -> Result<(Frontmatter, &str), String> {
    let (yaml, body) = split(source).ok_or_else(|| {
        "`SKILL.md` must start with YAML frontmatter between `---` lines".to_owned()
    })?;
    let frontmatter = serde_saphyr::from_str(yaml).map_err(|e| format!("frontmatter: {e}"))?;
    Ok((frontmatter, body.trim()))
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
