//! What an agent function declares for the current turn.

use crate::{Model, Skill};

/// What an agent function receives to make its declarations for the current turn.
#[derive(Debug, Default)]
pub struct AgentContext {
    /// Every `use_model` call this render, so a missing or second call can fail the run.
    models: Vec<Model>,
    instructions: Vec<String>,
    pub(crate) skills: Vec<Skill>,
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

    /// Mounts an already-resolved skill; nothing is read here.
    ///
    /// STUB: accepted and ignored until the skill design is decided.
    pub fn use_skill(&mut self, skill: Skill) {
        self.skills.push(skill);
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
