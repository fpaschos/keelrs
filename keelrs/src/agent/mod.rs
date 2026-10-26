//! Agents and what they declare each turn.

use std::fmt;
use std::sync::Arc;

pub use context::AgentContext;
pub use instance::{AgentInstance, Receipt, Reply, SubmissionId};

mod context;
mod instance;
pub(crate) mod rig;

type AgentFn = dyn Fn(&mut AgentContext) -> String + Send + Sync;

/// A named function that declares what the run uses and returns its instructions.
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

    /// Runs the agent function for one turn: its declarations and returned instructions.
    pub(crate) fn declare(&self) -> (AgentContext, String) {
        let mut cx = AgentContext::default();
        let returned = (self.f)(&mut cx);
        (cx, returned)
    }
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
