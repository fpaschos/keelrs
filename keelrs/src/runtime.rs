//! The runtime, handles to agent instances, and dispatching messages to them.
//!
//! Each agent instance is an actor (`agent::instance`); an
//! [`AgentInstance`] handle reaches it through its inbox.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::instance::{Command, InstanceTask};
use crate::{Agent, Error, Outcome, Result};

/// The running keelrs instance that knows the agents and hosts their instances.
///
/// Dropping it stops every agent instance: runs in flight and queued settle as
/// [`Outcome::Aborted`].
#[derive(Debug)]
pub struct Runtime {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    agents: HashMap<String, Agent>,
    next_submission: AtomicU64,
    /// Running instance tasks, keyed by `(agent name, instance id)`.
    instances: Mutex<HashMap<(String, String), mpsc::Sender<Command>>>,
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
#[derive(Debug, Clone)]
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
    ///
    /// The message runs after those dispatched before it; a model or connect
    /// error shows up on [`read`](Self::read), not here.
    pub async fn dispatch(&self, message: impl Into<String>) -> Result<Receipt> {
        let agent = self
            .runtime
            .agents
            .get(&self.agent)
            .ok_or_else(|| Error::UnknownAgent(self.agent.clone()))?;
        let submission_id =
            SubmissionId(self.runtime.next_submission.fetch_add(1, Ordering::Relaxed));
        // The first dispatch to this id spawns its task.
        let inbox = self
            .runtime
            .instances
            .lock()
            .unwrap()
            .entry(self.key())
            .or_insert_with(|| InstanceTask::spawn(agent.clone(), &self.runtime.shutdown))
            .clone();
        let command = Command::Dispatch {
            submission_id,
            text: message.into(),
        };
        if inbox.send(command).await.is_err() {
            return Err(aborted(submission_id));
        }
        Ok(Receipt { submission_id })
    }

    /// Waits for the receipt's message to settle and takes its reply.
    pub async fn read(&self, receipt: &Receipt) -> Result<Reply> {
        if !self.runtime.agents.contains_key(&self.agent) {
            return Err(Error::UnknownAgent(self.agent.clone()));
        }
        let inbox = self
            .runtime
            .instances
            .lock()
            .unwrap()
            .get(&self.key())
            .cloned()
            .ok_or_else(|| Error::InstanceNotFound(self.id.clone()))?;
        let submission_id = receipt.submission_id;
        let (respond_to, response) = oneshot::channel();
        let command = Command::Read {
            submission_id,
            respond_to,
        };
        if inbox.send(command).await.is_err() {
            return Err(aborted(submission_id));
        }
        // A task that stops before answering (it panicked) counts as aborted.
        response
            .await
            .unwrap_or_else(|_| Err(aborted(submission_id)))
    }

    fn key(&self) -> (String, String) {
        (self.agent.clone(), self.id.clone())
    }
}

fn aborted(submission_id: SubmissionId) -> Error {
    Error::Run {
        outcome: Outcome::Aborted,
        submission_id,
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

/// The claim ticket for one dispatched message: plain data, readable from any
/// handle to the instance that issued it.
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
    pub(crate) text: String,
    pub(crate) submission_id: SubmissionId,
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
