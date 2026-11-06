//! Agent instances: a cloneable [`AgentInstance`] handle and the actor behind it
//! (Alice Ryhl's "Actors with Tokio").
//!
//! The actor takes commands from its inbox one at a time, running each dispatch
//! to settled before the next. The canonical events ([`Event`]) go to durable
//! storage, marked `STORE:` below; until then the actor keeps each settled
//! result in memory to answer reads.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::InstanceAgent;
use crate::agent::rig::{self, Settled};
use crate::builtin::Caches;
use crate::builtin::structured::Structured;
use crate::event::{Event, EventId, Seq, StoredEvent};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// How many commands an instance's inbox holds before `dispatch` and `read` wait.
const INBOX_CAPACITY: usize = 1024;

/// One conversation with an agent, addressed by an id.
///
/// A handle to the instance's actor: clones reach the same instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    id: InstanceId,
    /// The actor's inbox.
    inbox: mpsc::Sender<Command>,
}

impl AgentInstance {
    /// Spawns the actor for instance `id` of `agent`, named `name`; `shutdown`
    /// cancelling aborts it.
    pub(crate) fn spawn(
        name: String,
        agent: InstanceAgent,
        id: InstanceId,
        shutdown: &CancellationToken,
    ) -> Self {
        let (inbox, receiver) = mpsc::channel(INBOX_CAPACITY);
        let actor = InstanceActor {
            agent,
            name,
            id: id.clone(),
            last: Seq::default(),
            inbox: receiver,
            cancel: shutdown.child_token(),
            settled: HashMap::new(),
            caches: Caches::default(),
        };
        tokio::spawn(actor.run());
        Self { id, inbox }
    }

    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &InstanceId {
        &self.id
    }

    /// Submits `message`; returns a receipt, not the reply.
    ///
    /// The message runs after those dispatched before it; a model or connect
    /// error shows up on [`read`](Self::read), not here.
    pub async fn dispatch(&self, message: impl Into<String>) -> Result<Receipt> {
        self.submit(message.into(), None).await
    }

    /// [`dispatch`](Self::dispatch) then [`read`](Self::read): submits `message`
    /// and waits for its reply.
    pub async fn ask(&self, message: impl Into<String>) -> Result<Reply> {
        let receipt = self.dispatch(message).await?;
        self.read(&receipt).await
    }

    /// Submits `message` and waits for a result of type `T`: the model must end by calling
    /// `finish_structured` with `T`'s schema. Plain text is re-asked up to 3 times, then the
    /// dispatch settles as [`Outcome::Failed`].
    pub async fn ask_structured<T>(&self, message: impl Into<String>) -> Result<T>
    where
        T: DeserializeOwned + JsonSchema + 'static,
    {
        let Receipt { dispatch_id } = self
            .submit(message.into(), Some(Structured::of::<T>()))
            .await?;
        let failed = |reason: String| Error::Run {
            outcome: Outcome::Failed { reason },
            dispatch_id,
        };
        // The JSON the model sent, which `finish_structured` checked deserializes as `T`.
        let value = self
            .settled(dispatch_id)
            .await?
            .structured
            .ok_or_else(|| failed("settled without a structured result".to_owned()))?;
        serde_json::from_value(value).map_err(|e| failed(e.to_string()))
    }

    /// Waits for the receipt's message to settle and takes its reply.
    pub async fn read(&self, receipt: &Receipt) -> Result<Reply> {
        let dispatch_id = receipt.dispatch_id;
        let settled = self.settled(dispatch_id).await?;
        Ok(Reply {
            text: settled.text,
            dispatch_id,
        })
    }

    /// Submits `text`, asking for `structured` when given.
    async fn submit(&self, text: String, structured: Option<Structured>) -> Result<Receipt> {
        let dispatch_id = DispatchId::fresh();
        let command = Command::Dispatch {
            dispatch_id,
            text,
            structured,
        };
        if self.inbox.send(command).await.is_err() {
            return Err(aborted(dispatch_id));
        }
        Ok(Receipt { dispatch_id })
    }

    /// Waits for `dispatch_id` to settle and takes what it settled with.
    async fn settled(&self, dispatch_id: DispatchId) -> Result<SettledValue> {
        let (respond_to, response) = oneshot::channel();
        let command = Command::Read {
            dispatch_id,
            respond_to,
        };
        if self.inbox.send(command).await.is_err() {
            return Err(aborted(dispatch_id));
        }
        // An actor that stops before answering (it panicked) counts as aborted.
        response.await.unwrap_or_else(|_| Err(aborted(dispatch_id)))
    }
}

fn aborted(dispatch_id: DispatchId) -> Error {
    Error::Run {
        outcome: Outcome::Aborted,
        dispatch_id,
    }
}

/// What an [`AgentInstance`] sends its actor, in order.
#[derive(Debug)]
enum Command {
    Dispatch {
        dispatch_id: DispatchId,
        text: String,
        /// The structured result the message asks for, if any.
        structured: Option<Structured>,
    },
    Read {
        dispatch_id: DispatchId,
        respond_to: oneshot::Sender<Result<SettledValue>>,
    },
}

/// What a dispatched message settled with when it produced a reply.
#[derive(Debug, Clone)]
pub(crate) struct SettledValue {
    /// The final assistant text.
    text: String,
    /// The structured result, when the message asked for one.
    structured: Option<Value>,
}

/// Runs one agent instance's dispatches, one at a time.
///
/// The inbox is the queue: commands run in the order they were sent, and a
/// dispatch runs to settled before the next command, so a read always finds its
/// message settled.
struct InstanceActor {
    agent: InstanceAgent,
    /// The agent's name, which with `id` names the instance's event stream.
    name: String,
    id: InstanceId,
    /// The `seq` of the instance's last event; this actor alone numbers them.
    last: Seq,
    inbox: mpsc::Receiver<Command>,
    /// The instance token: cancelling it aborts the run in flight and every later dispatch.
    cancel: CancellationToken,
    /// STORE: stands in for the stored `Event::Settled`s until durable storage keeps them.
    settled: HashMap<DispatchId, Result<SettledValue, Outcome>>,
    /// What this instance's built-in tools remember of its current sandbox; never stored.
    caches: Caches,
}

impl InstanceActor {
    /// Runs until every handle is gone.
    async fn run(mut self) {
        while let Some(command) = self.inbox.recv().await {
            match command {
                Command::Dispatch {
                    dispatch_id,
                    text,
                    structured,
                } => self.dispatch(dispatch_id, text, structured).await,
                Command::Read {
                    dispatch_id,
                    respond_to,
                } => {
                    // STORE: answer from the stored `Event::Settled` for `dispatch_id`.
                    let _ = respond_to.send(self.read(dispatch_id));
                }
            }
        }
    }

    /// Runs one dispatch to settled; a kill aborts it.
    async fn dispatch(
        &mut self,
        dispatch_id: DispatchId,
        text: String,
        structured: Option<Structured>,
    ) {
        // STORE: load the history from the stored `Event::Message`s.
        let history = Vec::new();
        let mut events = Vec::new();
        let run = rig::run(
            self.agent.clone(),
            text,
            history,
            structured,
            &mut self.caches,
        );
        let result = select! {
            biased;
            // Dropping the run's future stops its model call.
            () = self.cancel.cancelled() => Err(Outcome::Aborted),
            result = run => match result {
                Ok(Settled { reply, messages, structured }) => {
                    events.extend(messages.into_iter().map(Event::Message));
                    Ok(SettledValue { text: reply, structured })
                }
                Err(reason) => Err(Outcome::Failed { reason }),
            },
        };
        events.push(Event::Settled {
            dispatch_id,
            result: result.clone(),
        });
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        let stored: Vec<StoredEvent> = events
            .into_iter()
            .map(|event| {
                self.last = self.last.next();
                StoredEvent {
                    id: EventId::fresh(),
                    agent: self.name.clone(),
                    instance: self.id.clone(),
                    seq: self.last,
                    dispatch: Some(dispatch_id),
                    at,
                    event,
                }
            })
            .collect();
        // STORE: append `stored` to durable storage, before answering reads.
        drop(stored);
        self.settled.insert(dispatch_id, result);
    }

    /// What `dispatch_id` settled with; never pending, as the inbox runs
    /// every dispatch before a read sent after it.
    fn read(&self, dispatch_id: DispatchId) -> Result<SettledValue> {
        match self.settled.get(&dispatch_id) {
            Some(Ok(settled)) => Ok(settled.clone()),
            Some(Err(outcome)) => Err(Error::Run {
                outcome: outcome.clone(),
                dispatch_id,
            }),
            None => Err(Error::DispatchNotFound(dispatch_id)),
        }
    }
}

/// The claim ticket for one dispatched message: plain data, readable from any
/// handle to the instance that issued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    dispatch_id: DispatchId,
}

impl Receipt {
    /// The dispatched message this receipt claims.
    #[must_use]
    pub fn dispatch_id(&self) -> DispatchId {
        self.dispatch_id
    }
}

/// The settled outcome of one dispatched message.
#[derive(Debug, Clone)]
pub struct Reply {
    text: String,
    dispatch_id: DispatchId,
}

impl Reply {
    /// Final assistant text; empty when there is none.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The dispatched message this reply settles.
    #[must_use]
    pub fn dispatch_id(&self) -> DispatchId {
        self.dispatch_id
    }
}
