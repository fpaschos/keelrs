//! The runtime: the agents it knows and the agent instances it hosts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::{Agent, AgentInstance, Error, Result};

/// The running keelrs instance that knows the agents and hosts their instances.
///
/// Dropping it stops every agent instance: runs in flight and queued settle as
/// [`Outcome::Aborted`](crate::Outcome::Aborted).
#[derive(Debug)]
pub struct Runtime {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    agents: HashMap<String, Agent>,
    /// Instances already spawned, keyed by `(agent name, instance id)`, so the
    /// same id continues the same conversation.
    instances: Mutex<HashMap<(String, String), AgentInstance>>,
    /// Parent of every instance token; cancelled when the runtime is dropped.
    shutdown: CancellationToken,
}

impl Runtime {
    /// Starts a runtime hosting `agents`.
    pub fn start(agents: impl IntoIterator<Item = Agent>) -> Self {
        let agents = agents
            .into_iter()
            .map(|a| (a.name().to_owned(), a))
            .collect();
        Self {
            inner: Arc::new(Inner {
                agents,
                ..Inner::default()
            }),
        }
    }

    /// Addresses `agent` on this runtime; pick the instance with [`Init::id`],
    /// or get a new one with [`Init::fresh`].
    ///
    /// Never fails: an unknown agent errors on [`Init::id`].
    #[must_use]
    pub fn init(&self, agent: &Agent) -> Init {
        Init {
            runtime: self.inner.clone(),
            agent: agent.name().to_owned(),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

/// An agent on a runtime, waiting for an instance id.
#[derive(Debug)]
pub struct Init {
    runtime: Arc<Inner>,
    agent: String,
}

impl Init {
    /// The instance with this id: spawned on first use, continued after.
    ///
    /// Fails with [`Error::UnknownAgent`] when the agent was never given to
    /// [`Runtime::start`]. Must be called within a Tokio runtime.
    pub fn id(self, id: impl Into<String>) -> Result<AgentInstance> {
        let id = id.into();
        let agent = self
            .runtime
            .agents
            .get(&self.agent)
            .ok_or_else(|| Error::UnknownAgent(self.agent.clone()))?;
        let instance = self
            .runtime
            .instances
            .lock()
            .unwrap()
            .entry((self.agent, id.clone()))
            .or_insert_with(|| AgentInstance::spawn(agent.clone(), id, &self.runtime.shutdown))
            .clone();
        Ok(instance)
    }

    /// A new instance with a generated id: a UUIDv7, unique across restarts
    /// and ordered by time. Read it back with [`AgentInstance::id`].
    ///
    /// Fails as [`id`](Self::id) does.
    pub fn fresh(self) -> Result<AgentInstance> {
        self.id(uuid::Uuid::now_v7().to_string())
    }
}
