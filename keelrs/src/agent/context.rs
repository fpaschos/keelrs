//! What an agent function declares for the current turn.

use crate::{Model, Skill, Tool};

/// Tool names keelrs keeps for its built-in tools, with or without skills.
const RESERVED_TOOL_NAMES: [&str; 5] = [
    "activate_skill",
    "read_skill_resource",
    "task",
    "finish",
    "give_up",
];

/// What an agent function receives to make its declarations for the current turn.
#[derive(Debug, Default)]
pub struct AgentContext {
    /// Every `use_model` call this render, so a missing or second call can fail the run.
    models: Vec<Model>,
    instructions: Vec<String>,
    tools: Vec<Tool>,
    skills: Vec<Skill>,
}

impl AgentContext {
    /// Declares the model: a `provider/model` id, or a [`Model`] with options.
    ///
    /// Call it exactly once per render. No call, a second call or an empty id
    /// settles the dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_model(&mut self, model: impl Into<Model>) {
        self.models.push(model.into());
    }

    /// Adds text after the agent's returned instructions, in call order.
    ///
    /// Blank text settles the dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_instruction(&mut self, text: impl Into<String>) {
        self.instructions.push(text.into());
    }

    /// Declares a tool the model may call this turn, in call order.
    ///
    /// Two tools with one name, or a tool whose arguments are not a JSON
    /// object, settle the dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_tool(&mut self, tool: impl Into<Tool>) {
        self.tools.push(tool.into());
    }

    /// Mounts a skill this turn, in call order: the model sees its name and
    /// description and activates it by name with the built-in `activate_skill`.
    ///
    /// An invalid embedded skill, or two skills with one name, settle the
    /// dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_skill(&mut self, skill: impl Into<Skill>) {
        self.skills.push(skill.into());
    }

    /// Mounts each skill with [`use_skill`](Self::use_skill), in order.
    pub fn use_skills<I>(&mut self, skills: I)
    where
        I: IntoIterator,
        I::Item: Into<Skill>,
    {
        for skill in skills {
            self.use_skill(skill);
        }
    }

    /// The one model this render declared, or why the run fails.
    pub(crate) fn model(&self) -> Result<&Model, String> {
        match self.models.as_slice() {
            [] => Err("the agent function never called `use_model`".to_owned()),
            [model] if model.id.trim().is_empty() => {
                Err("`use_model` was given an empty model id".to_owned())
            }
            [model] => Ok(model),
            models => Err(format!(
                "`use_model` was called {} times in one render; call it once",
                models.len()
            )),
        }
    }

    /// The tools this render declared, or why the run fails.
    pub(crate) fn tools(&self) -> Result<&[Tool], String> {
        for (i, tool) in self.tools.iter().enumerate() {
            if RESERVED_TOOL_NAMES.contains(&&*tool.name) {
                return Err(format!(
                    "`{}` is reserved for keelrs's built-in tool",
                    tool.name
                ));
            }
            if self.tools[..i].iter().any(|other| other.name == tool.name) {
                return Err(format!(
                    "two tools are named `{}`; tool names must be unique",
                    tool.name
                ));
            }
            if tool.schema.get("type").and_then(|t| t.as_str()) != Some("object") {
                return Err(format!(
                    "`{}`'s arguments must be a JSON object; its schema is {}",
                    tool.name, tool.schema
                ));
            }
        }
        Ok(&self.tools)
    }

    /// The skills this render mounted, or why the run fails.
    pub(crate) fn skills(&self) -> Result<&[Skill], String> {
        for (i, skill) in self.skills.iter().enumerate() {
            skill.validate().map_err(|e| e.to_string())?;
            if self.skills[..i]
                .iter()
                .any(|other| other.name() == skill.name())
            {
                return Err(format!(
                    "two skills are named `{}`; skill names must be unique",
                    skill.name()
                ));
            }
        }
        Ok(&self.skills)
    }

    /// This turn's instructions: the returned text, then each `use_instruction`
    /// text, joined by a blank line; `None` when all is empty.
    pub(crate) fn instructions(&self, returned: &str) -> Result<Option<String>, String> {
        if self.instructions.iter().any(|text| text.trim().is_empty()) {
            return Err("`use_instruction` was given empty text".to_owned());
        }
        let texts: Vec<&str> = std::iter::once(returned)
            .filter(|returned| !returned.is_empty())
            .chain(self.instructions.iter().map(String::as_str))
            .collect();
        Ok((!texts.is_empty()).then(|| texts.join("\n\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::AgentContext;

    #[test]
    fn returned_text_comes_first_then_each_use_instruction_in_call_order() {
        let mut cx = AgentContext::default();
        cx.use_instruction("Second.");
        cx.use_instruction("Third.");
        assert_eq!(
            cx.instructions("First."),
            Ok(Some("First.\n\nSecond.\n\nThird.".to_owned()))
        );
    }

    #[test]
    fn an_empty_returned_text_is_skipped() {
        let mut cx = AgentContext::default();
        cx.use_instruction("Only.");
        assert_eq!(cx.instructions(""), Ok(Some("Only.".to_owned())));
    }

    #[test]
    fn no_instructions_at_all_is_none() {
        let cx = AgentContext::default();
        assert_eq!(cx.instructions(""), Ok(None));
    }

    #[test]
    fn duplicate_texts_are_kept() {
        let mut cx = AgentContext::default();
        cx.use_instruction("Again.");
        cx.use_instruction("Again.");
        assert_eq!(
            cx.instructions("Again."),
            Ok(Some("Again.\n\nAgain.\n\nAgain.".to_owned()))
        );
    }
}
