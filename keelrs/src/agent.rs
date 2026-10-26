//! Agents and what they declare each turn.

use std::fmt;
use std::sync::Arc;

use crate::{Model, Skill};

type AgentFn = dyn Fn(&mut AgentContext) -> String + Send + Sync;

/// A named function that declares what the run uses and returns the prompt.
///
/// It runs again before every turn and must return synchronously, as in Flue:
/// async work lives in tools and resource factories.
#[derive(Clone)]
pub struct Agent {
    name: String,
    f: Arc<AgentFn>,
}

impl Agent {
    /// An agent named `name` whose function `f` runs before every turn.
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&mut AgentContext) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            f: Arc::new(f),
        }
    }

    /// The name the runtime knows this agent by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Runs the agent function for one turn.
    pub(crate) fn declare(&self) -> (AgentContext, String) {
        let mut cx = AgentContext::default();
        let prompt = (self.f)(&mut cx);
        (cx, prompt)
    }
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// What an agent function receives to make its declarations for the current turn.
#[derive(Debug, Default)]
pub struct AgentContext {
    /// Every `use_model` call this render, so a missing or second call can fail the run.
    models: Vec<Model>,
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
}
