//! The runtime, agent instances, and dispatching messages to them.
//!
//! STUB: no model is called and nothing is persisted. A dispatch runs the agent
//! function and settles at once with a reply that echoes what the agent declared.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{Agent, Error, Result};

/// The running keelrs instance that knows the agents and hosts their instances.
#[derive(Debug)]
pub struct Runtime {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    agents: HashMap<String, Agent>,
    next_submission: AtomicU64,
    /// Settled replies, per instance, keyed by `(agent name, instance id)`.
    instances: Mutex<HashMap<(String, String), HashMap<SubmissionId, Reply>>>,
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

    /// Addresses `agent` on this runtime; pick the instance with [`Init::id`].
    ///
    /// Creates nothing and never fails: an unknown agent errors on dispatch.
    #[must_use]
    pub fn init(&self, agent: &Agent) -> Init {
        Init {
            runtime: self.inner.clone(),
            agent: agent.name().to_owned(),
        }
    }
}

/// An agent on a runtime, waiting for an instance id.
#[derive(Debug)]
pub struct Init {
    runtime: Arc<Inner>,
    agent: String,
}

impl Init {
    /// The instance with this id; the first dispatch creates it, later ones continue it.
    pub fn id(self, id: impl Into<String>) -> AgentInstance {
        AgentInstance {
            runtime: self.runtime,
            agent: self.agent,
            id: id.into(),
        }
    }
}

/// One conversation with an agent, addressed by an id.
#[derive(Debug)]
pub struct AgentInstance {
    runtime: Arc<Inner>,
    agent: String,
    id: String,
}

impl AgentInstance {
    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Submits `message`; returns a receipt, not the reply.
    pub async fn dispatch(&self, message: impl Into<String>) -> Result<Receipt> {
        let agent = self
            .runtime
            .agents
            .get(&self.agent)
            .ok_or_else(|| Error::UnknownAgent(self.agent.clone()))?;
        let submission_id =
            SubmissionId(self.runtime.next_submission.fetch_add(1, Ordering::Relaxed));

        let (cx, prompt) = agent.declare();
        let skills: Vec<&str> = cx.skills.iter().map(|s| s.name()).collect();
        let text = format!(
            "[stub {} | instance {} | skills {skills:?}] system: {prompt:?} user: {:?}",
            cx.model.as_deref().unwrap_or("<no model>"),
            self.id,
            message.into(),
        );

        self.runtime
            .instances
            .lock()
            .unwrap()
            .entry((self.agent.clone(), self.id.clone()))
            .or_default()
            .insert(
                submission_id,
                Reply {
                    text,
                    submission_id,
                },
            );
        Ok(Receipt { submission_id })
    }

    /// Waits for the receipt's message to settle and takes its reply.
    pub async fn read(&self, receipt: &Receipt) -> Result<Reply> {
        if !self.runtime.agents.contains_key(&self.agent) {
            return Err(Error::UnknownAgent(self.agent.clone()));
        }
        let instances = self.runtime.instances.lock().unwrap();
        let replies = instances
            .get(&(self.agent.clone(), self.id.clone()))
            .ok_or_else(|| Error::InstanceNotFound(self.id.clone()))?;
        replies
            .get(&receipt.submission_id)
            .cloned()
            .ok_or(Error::SubmissionNotFound(receipt.submission_id))
    }
}

/// Identifies one dispatched message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmissionId(u64);

impl fmt::Display for SubmissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The claim ticket for one dispatched message: plain data, readable from any instance handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    submission_id: SubmissionId,
}

impl Receipt {
    /// The dispatched message this receipt claims.
    #[must_use]
    pub fn submission_id(&self) -> SubmissionId {
        self.submission_id
    }
}

/// The settled outcome of one dispatched message.
#[derive(Debug, Clone)]
pub struct Reply {
    text: String,
    submission_id: SubmissionId,
}

impl Reply {
    /// Final assistant text; empty when there is none.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The dispatched message this reply settles.
    #[must_use]
    pub fn submission_id(&self) -> SubmissionId {
        self.submission_id
    }
}
