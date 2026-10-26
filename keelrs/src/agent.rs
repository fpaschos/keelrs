//! Agents and what they declare each turn.

use std::fmt;
use std::sync::Arc;

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
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&mut AgentContext) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            f: Arc::new(f),
        }
    }

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
    pub(crate) model: Option<String>,
}

impl AgentContext {
    /// Declares the model, as `provider/model`.
    pub fn use_model(&mut self, model: impl Into<String>) {
        self.model = Some(model.into());
    }
}
