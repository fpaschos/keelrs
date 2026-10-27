//! The built-in skill tools, `activate_skill` and `read_skill_resource`: ordinary
//! [`Tool`]s built each render over that render's skills, so they run on the
//! same tool loop as `use_tool` tools and are pinned with them.

use std::borrow::Cow;
use std::convert::Infallible;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
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
    Tool::builtin(
        "activate_skill",
        "Load a skill's full instructions by name before doing a task it describes.",
        move |call: ToolCall<Activate>| async move {
            Ok::<_, Infallible>(activation(&skills, &call.data.name))
        },
    )
}

/// Flue's activation block for the skill `name`, or which skills there are.
fn activation(skills: &[Skill], name: &str) -> String {
    let skill = match find(skills, name) {
        Ok(skill) => skill,
        Err(miss) => return miss,
    };
    let mut text = format!(
        "Run the skill named \"{name}\".\n\n<skill_instructions>\n{}\n</skill_instructions>",
        skill.instructions()
    );
    let resources: Vec<String> = skill.files().map(|path| format!("- {path}")).collect();
    if !resources.is_empty() {
        text.push_str(&format!(
            "\n\nSupporting skill resources are available but are not loaded into context unless needed:\n\
             <skill_resources>\n{}\n</skill_resources>",
            resources.join("\n")
        ));
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
    Tool::builtin(
        "read_skill_resource",
        "Read a supporting file of an activated skill, paged by line.",
        move |call: ToolCall<Read>| async move {
            Ok::<_, Infallible>(resource(&skills, &call.data).await)
        },
    )
}

/// The requested page of a skill's file, or why it cannot be read.
async fn resource(skills: &[Skill], read: &Read) -> String {
    let skill = match find(skills, &read.skill) {
        Ok(skill) => skill,
        Err(miss) => return miss,
    };
    let path = match super::normalize(&read.path) {
        Ok(path) => path,
        Err(refusal) => return refusal,
    };
    let bytes = match skill.read(&path).await {
        Some(Ok(bytes)) => bytes,
        Some(Err(refusal)) => return refusal,
        None => {
            let files: Vec<&str> = skill.files().collect();
            return format!(
                "`{path}` is not a file of skill \"{}\"; its files: {}",
                read.skill,
                files.join(", ")
            );
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(base64(&bytes)),
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
    let mut cut = None;
    for line in lines.iter().skip(offset - 1).take(limit) {
        bytes += line.len() + 1;
        if bytes > MAX_BYTES {
            if shown.is_empty() {
                // One line wider than a page: show its start rather than all of it.
                cut = Some(line.len());
                shown.push(&line[..line.floor_char_boundary(MAX_BYTES)]);
            }
            break;
        }
        shown.push(*line);
    }
    let end = offset - 1 + shown.len();
    let mut page = shown.join("\n");
    if let Some(width) = cut {
        page.push_str(&format!(
            "\n\n[Line {offset} is cut at {} of {width} bytes.]",
            page.len()
        ));
    }
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
    let encoded = STANDARD.encode(bytes);
    let lines: Vec<_> = encoded
        .as_bytes()
        .chunks(76)
        .map(String::from_utf8_lossy)
        .collect();
    lines.join("\n")
}

/// The mounted skill `name`, or the model-facing miss naming the available ones.
fn find<'a>(skills: &'a [Skill], name: &str) -> Result<&'a Skill, String> {
    skills
        .iter()
        .find(|skill| skill.name() == name)
        .ok_or_else(|| {
            let names: Vec<&str> = skills.iter().map(Skill::name).collect();
            format!(
                "skill \"{name}\" is not available; available skills: {}",
                names.join(", ")
            )
        })
}
