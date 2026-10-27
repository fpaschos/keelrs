//! The skill catalog: what the model sees of each mounted skill until it activates one.

use super::Skill;

/// Flue's `## Available Skills` section, one line per skill in mount order;
/// `None` without skills. Rebuilt every turn from that render's skills.
pub(crate) fn catalog(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let lines: Vec<String> = skills
        .iter()
        .map(|skill| {
            // One line per skill, whatever YAML block style the description used.
            let description: Vec<&str> = skill.description().split_whitespace().collect();
            format!("- **{}** — {}", skill.name(), description.join(" "))
        })
        .collect();
    Some(format!(
        "## Available Skills\n\n\
         When a task matches a skill description, call the `activate_skill` tool with \
         that skill name before proceeding so its full instructions are loaded.\n\n{}",
        lines.join("\n")
    ))
}
