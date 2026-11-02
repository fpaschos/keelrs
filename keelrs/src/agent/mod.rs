//! Agents and what they declare each turn.

use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use context::Declared;
pub use context::{AgentContext, BuiltinTools};
pub use id::InstanceId;
pub use instance::{AgentInstance, Receipt, Reply, SubmissionId};

mod context;
mod id;
mod instance;
pub(crate) mod rig;

type AgentFn<D> = dyn Fn(&mut AgentContext<D>) -> String + Send + Sync;

/// Model calls one dispatch may make unless [`Agent::max_turns`] says otherwise.
const DEFAULT_MAX_TURNS: usize = 100;

/// A named function that declares what the run uses and returns its instructions.
///
/// It runs again before every turn and must return synchronously, as in Flue:
/// async work lives in tools and resource factories. `D` is its
/// [instance data](AgentContext::data), `()` for none.
pub struct Agent<D = ()> {
    name: String,
    f: Arc<AgentFn<D>>,
    max_turns: usize,
}

impl<D> Agent<D>
where
    D: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// An agent named `name` whose function `f` runs before every turn.
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&mut AgentContext<D>) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            f: Arc::new(f),
            max_turns: DEFAULT_MAX_TURNS,
        }
    }
}

impl<D> Agent<D> {
    /// Caps the model calls of one dispatch (default 100); reaching the cap
    /// settles it as [`Outcome::Failed`](crate::Outcome::Failed).
    #[must_use]
    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// The name the runtime knows this agent by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether `other` is this very agent, not just one with its name.
    pub(crate) fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.f, &other.f)
    }
}

impl<D: Send + Sync + 'static> Agent<D> {
    /// This agent for instance `id`, its data frozen for the instance's life.
    pub(crate) fn bind(&self, id: InstanceId, data: D) -> Bound {
        let (f, data) = (self.f.clone(), Arc::new(data));
        Bound {
            max_turns: self.max_turns,
            render: Arc::new(move || {
                let mut cx = AgentContext::new(data.clone(), id.clone());
                let returned = f(&mut cx);
                (cx.into_declared(), returned)
            }),
        }
    }
}

impl<D> Clone for Agent<D> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            f: self.f.clone(),
            max_turns: self.max_turns,
        }
    }
}

impl<D> fmt::Debug for Agent<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// An agent bound to one instance's id and data: what the run loop renders.
#[derive(Clone)]
pub(crate) struct Bound {
    pub(crate) max_turns: usize,
    render: Arc<dyn Fn() -> (Declared, String) + Send + Sync>,
}

impl Bound {
    /// Runs the agent function for one turn: its declarations and returned instructions.
    pub(crate) fn declare(&self) -> (Declared, String) {
        (self.render)()
    }
}
