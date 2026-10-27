//! The built-in skill tools, `activate_skill` and `read_skill_resource`: ordinary
//! [`Tool`]s built each render over that render's skills, so they run on the
//! same tool loop as `use_tool` tools and are pinned with them.

use std::convert::Infallible;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;

use super::Skill;
use crate::{Tool, ToolCall};

/// The built-in tools for `skills`: `activate_skill` with at least one skill,
/// `read_skill_resource` when one has files besides `SKILL.md`.
pub(crate) fn builtin(skills: &[Skill]) -> Vec<Tool> {
    if skills.is_empty() {
        return Vec::new();
    }
    let skills: Arc<[Skill]> = skills.into();
    let mut tools = vec![activate_skill(skills.clone())];
    if skills.iter().any(|skill| skill.files().next().is_some()) {
        tools.push(read_skill_resource(skills));
    }
    tools
}

#[derive(Deserialize, JsonSchema)]
struct Activate {
    /// The skill's name, as listed under Available Skills.
    name: String,
}

fn activate_skill(skills: Arc<[Skill]>) -> Tool {
    Tool::new(
        "activate_skill",
        "Load a skill's full instructions by name before doing a task it describes.",
        move |call: ToolCall<Activate>| async move {
            Ok::<_, Infallible>(activation(&skills, &call.data.name))
        },
    )
}

/// Flue's activation block for the skill `name`, or which skills there are.
fn activation(skills: &[Skill], name: &str) -> String {
    let Some(skill) = skills.iter().find(|skill| skill.name() == name) else {
        return format!(
            "skill \"{name}\" is not available; available skills: {}",
            names(skills)
        );
    };
    let mut text = format!(
        "Run the skill named \"{name}\".\n\n<skill_instructions>\n{}\n</skill_instructions>",
        skill.instructions()
    );
    let resources: Vec<String> = skill.files().map(|(path, _)| format!("- {path}")).collect();
    if !resources.is_empty() {
        text.push_str(
            "\n\nSupporting skill resources are available but are not loaded into context unless needed:\n\
             <skill_resources>\n",
        );
        text.push_str(&resources.join("\n"));
        text.push_str("\n</skill_resources>");
    }
    text
}

/// Lines one read returns at most.
const MAX_LINES: usize = 2000;
/// Bytes one read returns at most, counting each line's newline.
const MAX_BYTES: usize = 50 * 1024;

#[derive(Deserialize, JsonSchema)]
struct Read {
    /// The skill's name.
    skill: String,
    /// The file's path relative to the skill's folder, as listed on activation.
    path: String,
    /// The line to start at, from 1.
    offset: Option<usize>,
    /// How many lines to read.
    limit: Option<usize>,
}

fn read_skill_resource(skills: Arc<[Skill]>) -> Tool {
    Tool::new(
        "read_skill_resource",
        "Read a supporting file of an activated skill, paged by line.",
        move |call: ToolCall<Read>| async move { Ok::<_, Infallible>(resource(&skills, &call.data)) },
    )
}

/// The requested page of a skill's file, or why it cannot be read.
fn resource(skills: &[Skill], read: &Read) -> String {
    let Some(skill) = skills.iter().find(|skill| skill.name() == read.skill) else {
        return format!(
            "skill \"{}\" is not available; available skills: {}",
            read.skill,
            names(skills)
        );
    };
    let path = read.path.strip_prefix("./").unwrap_or(&read.path);
    if let Err(refusal) = super::check_path(path) {
        return refusal;
    }
    let Some((_, bytes)) = skill.files().find(|(file, _)| *file == path) else {
        let files: Vec<&str> = skill.files().map(|(file, _)| file).collect();
        return format!(
            "`{path}` is not a file of skill \"{}\"; its files: {}",
            read.skill,
            files.join(", ")
        );
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => base64(bytes),
    };
    page(&text, read.offset.unwrap_or(1).max(1), read.limit)
}

/// Lines `offset..` of `text`, at most `limit`, 2000 and 50 KB, with Flue's
/// footer when more follow.
fn page(text: &str, offset: usize, limit: Option<usize>) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if offset > total.max(1) {
        return format!("offset {offset} is past the end of the file ({total} lines)");
    }
    let limit = limit.unwrap_or(MAX_LINES).clamp(1, MAX_LINES);
    let mut shown = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().skip(offset - 1).take(limit) {
        bytes += line.len() + 1;
        if bytes > MAX_BYTES && !shown.is_empty() {
            break;
        }
        shown.push(*line);
    }
    let end = offset - 1 + shown.len();
    let mut page = shown.join("\n");
    if end < total {
        page.push_str(&format!(
            "\n\n[Showing lines {offset}-{end} of {total}. Use offset={} to continue.]",
            end + 1
        ));
    }
    page
}

/// Standard base64 (RFC 4648, padded), wrapped at 76 columns.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                encoded.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                encoded.push('=');
            }
        }
    }
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(76)
        .map(|line| std::str::from_utf8(line).unwrap_or_default())
        .collect();
    lines.join("\n")
}

fn names(skills: &[Skill]) -> String {
    skills
        .iter()
        .map(Skill::name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_the_rfc_4648_test_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, encoded) in vectors {
            assert_eq!(base64(input.as_bytes()), encoded, "{input:?}");
        }
    }
}
