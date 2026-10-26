//! Agent instances: a cloneable [`AgentInstance`] handle and the actor behind it
//! (Alice Ryhl's "Actors with Tokio").
//!
//! The actor takes commands from its inbox one at a time, running each dispatch
//! to settled before the next. It keeps no events: the canonical events
//! ([`Event`]) go to durable storage, marked `STORE:` below.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::rig::{self, Settled};
use crate::event::Event;
use crate::{Agent, Error, Outcome, Result};

/// How many commands an instance's inbox holds before `dispatch` and `read` wait.
const INBOX_CAPACITY: usize = 1024;

/// Submission ids are unique across every instance, so a receipt read on the
/// wrong instance is never mistaken for one of its own messages.
static NEXT_SUBMISSION: AtomicU64 = AtomicU64::new(0);

/// One conversation with an agent, addressed by an id.
///
/// A handle to the instance's actor: clones reach the same instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    id: String,
    /// The actor's inbox.
    inbox: mpsc::Sender<Command>,
}

impl AgentInstance {
    /// Spawns the actor for instance `id` of `agent`; `shutdown` cancelling aborts it.
    pub(crate) fn spawn(agent: Agent, id: String, shutdown: &CancellationToken) -> Self {
        let (inbox, receiver) = mpsc::channel(INBOX_CAPACITY);
        let actor = InstanceActor {
            agent,
            inbox: receiver,
            cancel: shutdown.child_token(),
        };
        tokio::spawn(actor.run());
        Self { id, inbox }
    }

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
        let submission_id = SubmissionId(NEXT_SUBMISSION.fetch_add(1, Ordering::Relaxed));
        let command = Command::Dispatch {
            submission_id,
            text: message.into(),
        };
        if self.inbox.send(command).await.is_err() {
            return Err(aborted(submission_id));
        }
        Ok(Receipt { submission_id })
    }

    /// Waits for the receipt's message to settle and takes its reply.
    pub async fn read(&self, receipt: &Receipt) -> Result<Reply> {
        let submission_id = receipt.submission_id;
        let (respond_to, response) = oneshot::channel();
        let command = Command::Read {
            submission_id,
            respond_to,
        };
        if self.inbox.send(command).await.is_err() {
            return Err(aborted(submission_id));
        }
        // An actor that stops before answering (it panicked) counts as aborted.
        response
            .await
            .unwrap_or_else(|_| Err(aborted(submission_id)))
    }
}

fn aborted(submission_id: SubmissionId) -> Error {
    Error::Run {
        outcome: Outcome::Aborted,
        submission_id,
    }
}

/// What an [`AgentInstance`] sends its actor, in order.
#[derive(Debug)]
enum Command {
    Dispatch {
        submission_id: SubmissionId,
        text: String,
    },
    Read {
        submission_id: SubmissionId,
        respond_to: oneshot::Sender<Result<Reply>>,
    },
}

/// Runs one agent instance's dispatches, one at a time.
///
/// The inbox is the queue: commands run in the order they were sent, and a
/// dispatch runs to settled before the next command, so a read always finds its
/// message settled.
struct InstanceActor {
    agent: Agent,
    inbox: mpsc::Receiver<Command>,
    /// The instance token: cancelling it aborts the run in flight and every later dispatch.
    cancel: CancellationToken,
}

impl InstanceActor {
    /// Runs until every handle is gone.
    async fn run(mut self) {
        while let Some(command) = self.inbox.recv().await {
            match command {
                Command::Dispatch {
                    submission_id,
                    text,
                } => self.dispatch(submission_id, text).await,
                Command::Read {
                    submission_id,
                    respond_to,
                } => {
                    // STORE: answer from the stored `Event::Settled` for `submission_id`.
                    let _ = respond_to.send(Err(Error::SubmissionNotFound(submission_id)));
                }
            }
        }
    }

    /// Runs one dispatch to settled; a kill aborts it.
    async fn dispatch(&self, submission_id: SubmissionId, text: String) {
        // STORE: load the history from the stored `Event::Message`s.
        let history = Vec::new();
        let mut events = Vec::new();
        let result = select! {
            biased;
            // Dropping the run's future stops its model call.
            () = self.cancel.cancelled() => Err(Outcome::Aborted),
            result = rig::run(self.agent.clone(), text, history) => match result {
                Ok(Settled { reply, messages }) => {
                    events.extend(messages.into_iter().map(Event::Message));
                    Ok(reply)
                }
                Err(reason) => Err(Outcome::Failed { reason }),
            },
        };
        events.push(Event::Settled {
            submission_id,
            result,
        });
        // STORE: append `events` to durable storage, before answering reads.
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
