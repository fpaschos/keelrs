//! What an agent function declares for the current turn.

use std::sync::Arc;

use crate::builtin::NAMES as BUILTIN_NAMES;
use crate::sandbox::Sandbox;
use crate::{InstanceId, Model, Skill, Tool};

/// Tool names keelrs keeps for its skill tools, with or without skills; the
/// sandbox built-ins' names are kept too.
const SKILL_TOOL_NAMES: [&str; 2] = ["activate_skill", "read_skill_resource"];

/// What an agent function receives to make its declarations for the current turn,
/// with its instance's id and [data](Self::data).
#[derive(Debug)]
pub struct AgentContext<D = ()> {
    data: Arc<D>,
    id: InstanceId,
    declared: Declared,
}

/// What one render declared, read by the run loop.
#[derive(Debug, Default)]
pub(crate) struct Declared {
    /// Every `use_model` call this render, so a missing or second call can fail the run.
    models: Vec<Model>,
    instructions: Vec<String>,
    tools: Vec<Tool>,
    skills: Vec<Skill>,
    /// Every `use_sandbox` sandbox this render declared, in call order.
    sandboxes: Vec<Sandbox>,
    /// The built-in tools `only` kept, when it was called.
    only: Option<Vec<String>>,
}

/// The built-in tools a sandbox brings to a turn: `read`, `write`, `edit`,
/// `bash`, `grep`, `glob`, `delete` and `move`, all of them unless
/// narrowed with [`only`](Self::only). Returned by
/// [`AgentContext::use_sandbox`].
#[derive(Debug)]
pub struct BuiltinTools<'a> {
    only: &'a mut Option<Vec<String>>,
}

impl BuiltinTools<'_> {
    /// Keeps only the built-in tools `names`, such as
    /// `["read", "grep", "glob", "bash"]` for an agent that looks but does
    /// not edit (its `bash` can still write); `[]` keeps none.
    ///
    /// An unknown name settles the dispatch as
    /// [`Outcome::Failed`](crate::Outcome::Failed). The names left out stay
    /// reserved: no declared tool may take them.
    pub fn only<'n>(self, names: impl IntoIterator<Item = &'n str>) {
        *self.only = Some(names.into_iter().map(str::to_owned).collect());
    }
}

impl<D> AgentContext<D> {
    pub(crate) fn new(data: Arc<D>, id: InstanceId) -> Self {
        Self {
            data,
            id,
            declared: Declared::default(),
        }
    }

    /// What this render declared.
    pub(crate) fn into_declared(self) -> Declared {
        self.declared
    }

    /// This instance's data, given when it was created and fixed for its life.
    #[must_use]
    pub fn data(&self) -> &D {
        &self.data
    }

    /// This instance's id.
    #[must_use]
    pub fn id(&self) -> &InstanceId {
        &self.id
    }

    /// Declares the model: a `provider/model` id, or a [`Model`] with options.
    ///
    /// Call it exactly once per render. No call, a second call or an empty id
    /// settles the dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_model(&mut self, model: impl Into<Model>) {
        self.declared.models.push(model.into());
    }

    /// Adds text after the agent's returned instructions, in call order.
    ///
    /// Blank text settles the dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_instruction(&mut self, text: impl Into<String>) {
        self.declared.instructions.push(text.into());
    }

    /// Declares a tool the model may call this turn, in call order.
    ///
    /// Two tools with one name, or a tool whose arguments are not a JSON
    /// object, settle the dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_tool(&mut self, tool: impl Into<Tool>) {
        self.declared.tools.push(tool.into());
    }

    /// Declares each tool with [`use_tool`](Self::use_tool), in order.
    pub fn use_tools<I>(&mut self, tools: I)
    where
        I: IntoIterator,
        I::Item: Into<Tool>,
    {
        for tool in tools {
            self.use_tool(tool);
        }
    }

    /// Declares the sandbox this turn's sandboxed tools act through
    /// ([`Tool::sandbox`]), with keelrs's built-in tools over it, listed
    /// before skill tools and declared tools; narrow them with
    /// [`BuiltinTools::only`]. keelrs holds it only for this dispatch.
    ///
    /// Call it exactly once per render. No call or a second call settles the
    /// dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_sandbox(&mut self, sandbox: &Sandbox) -> BuiltinTools<'_> {
        self.declared.sandboxes.push(sandbox.clone());
        BuiltinTools {
            only: &mut self.declared.only,
        }
    }

    /// Mounts a skill this turn, in call order: the model sees its name and
    /// description and activates it by name with the built-in `activate_skill`.
    ///
    /// An invalid embedded skill, or two skills with one name, settle the
    /// dispatch as [`Outcome::Failed`](crate::Outcome::Failed).
    pub fn use_skill(&mut self, skill: impl Into<Skill>) {
        self.declared.skills.push(skill.into());
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
}

impl Declared {
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
            if SKILL_TOOL_NAMES.contains(&&*tool.name) || BUILTIN_NAMES.contains(&&*tool.name) {
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

    /// The one sandbox this render declared, or why the run fails.
    pub(crate) fn sandbox(&self) -> Result<&Sandbox, String> {
        match self.sandboxes.as_slice() {
            [] => Err("the agent function never called `use_sandbox`".to_owned()),
            [sandbox] => Ok(sandbox),
            sandboxes => Err(format!(
                "`use_sandbox` was called {} times in one render; call it once",
                sandboxes.len()
            )),
        }
    }

    /// The names of the built-in tools this render gets, in [`BUILTIN_NAMES`] order:
    /// all of them unless `only` narrowed them.
    pub(crate) fn builtin_names(&self) -> Result<Vec<&'static str>, String> {
        let Some(kept) = &self.only else {
            return Ok(BUILTIN_NAMES.to_vec());
        };
        if let Some(unknown) = kept
            .iter()
            .find(|name| !BUILTIN_NAMES.contains(&name.as_str()))
        {
            return Err(format!(
                "`only` names `{unknown}`, which is not a built-in tool; they are {}",
                BUILTIN_NAMES.join(", ")
            ));
        }
        Ok(BUILTIN_NAMES
            .into_iter()
            .filter(|name| kept.iter().any(|kept| kept == name))
            .collect())
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
    use super::*;

    #[test]
    fn returned_text_comes_first_then_each_use_instruction_in_call_order() {
        let mut cx = AgentContext::new(Arc::new(()), InstanceId::new("test"));
        cx.use_instruction("Second.");
        cx.use_instruction("Third.");
        assert_eq!(
            cx.declared.instructions("First."),
            Ok(Some("First.\n\nSecond.\n\nThird.".to_owned()))
        );
    }

    #[test]
    fn an_empty_returned_text_is_skipped() {
        let mut cx = AgentContext::new(Arc::new(()), InstanceId::new("test"));
        cx.use_instruction("Only.");
        assert_eq!(cx.declared.instructions(""), Ok(Some("Only.".to_owned())));
    }

    #[test]
    fn no_instructions_at_all_is_none() {
        let cx = AgentContext::new(Arc::new(()), InstanceId::new("test"));
        assert_eq!(cx.declared.instructions(""), Ok(None));
    }

    #[test]
    fn duplicate_texts_are_kept() {
        let mut cx = AgentContext::new(Arc::new(()), InstanceId::new("test"));
        cx.use_instruction("Again.");
        cx.use_instruction("Again.");
        assert_eq!(
            cx.declared.instructions("Again."),
            Ok(Some("Again.\n\nAgain.\n\nAgain.".to_owned()))
        );
    }
}
