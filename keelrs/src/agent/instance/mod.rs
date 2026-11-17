//! Agent instances: a cloneable [`AgentInstance`] handle and the actor behind it
//! (Alice Ryhl's "Actors with Tokio").
//!
//! The actor is the only writer of its instance's event stream, while it
//! holds the stream's lease. It stores each dispatch before returning its
//! receipt, runs dispatches one at a time, each as one piped [`Run`](crate::agent::rig::Run) that
//! stores what it produced through the actor before it goes on, and answers
//! reads from what the stream adds up to ([`fold::Folded`]).
//!
//! This file holds the handle; `actor.rs` the actor; `fold.rs` the fold;
//! `lease.rs` the lease the actor holds.
//! The handle holds the actor's [`ActorRef`] for good: after the actor
//! stops, the next call starts it again in place.

mod actor;
mod fold;
mod lease;

use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use self::actor::{InstanceActor, InstanceArgs};
use crate::actors::ActorRef;
use crate::actors::entity::Entities;
use crate::agent::TurnSetup;
use crate::agent::rig::{Connect, RunCompleted};
use crate::builtin::structured::StructuredSpec;
use crate::event::Event;
use crate::store::Store;
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// Names an instance by its agent's name and its id.
type InstanceKey = (String, InstanceId);

/// Every instance's actor, by key.
pub(crate) struct Instances {
    entities: Entities<InstanceKey, InstanceActor>,
    /// Connects the model each run declares.
    connect: Connect,
    store: Store,
}

impl Instances {
    /// No instances yet; their actors run on `rt` until `shutdown` is cancelled.
    pub(crate) fn new(
        rt: tokio::runtime::Handle,
        shutdown: CancellationToken,
        connect: Connect,
        store: Store,
    ) -> Self {
        Self {
            entities: Entities::new(rt, shutdown),
            connect,
            store,
        }
    }

    /// Instance `id` of the agent named `agent`, its turns set up by
    /// `setup`, created with `data` (as JSON). Its actor starts nothing until
    /// its first call.
    pub(crate) fn get(
        &self,
        agent: String,
        id: InstanceId,
        setup: TurnSetup,
        data: Value,
    ) -> AgentInstance {
        let key = (agent, id);
        let data = Arc::new(data);
        let args = Arc::new(InstanceArgs {
            name: key.0.clone(),
            id: key.1.clone(),
            setup,
            data: Arc::clone(&data),
            connect: self.connect.clone(),
            store: self.store.clone(),
        });
        let r = self.entities.get(key.clone(), args);
        AgentInstance { key, data, r }
    }
}

/// One conversation with an agent, addressed by an id.
///
/// A handle to the instance's actor: clones reach the same instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    key: InstanceKey,
    /// The data (as JSON) this handle was given; checked on each dispatch.
    data: Arc<Value>,
    r: ActorRef<Command>,
}

impl AgentInstance {
    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &InstanceId {
        &self.key.1
    }

    /// Submits `message`; returns a receipt, not the reply.
    ///
    /// The message is stored before the receipt returns, and runs after those
    /// dispatched before it. A store error, or another runtime holding the
    /// instance, shows up here; a model or connect error on [`read`](Self::read).
    /// Fails with [`Error::InstanceDataConflict`] when the instance exists
    /// with other data than this handle's.
    pub async fn dispatch(&self, message: impl Into<String>) -> Result<Receipt> {
        self.submit(message.into(), None).await
    }

    /// [`dispatch`](Self::dispatch) then [`read`](Self::read): submits `message`
    /// and waits for its reply. Fails as both do.
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
            .submit(message.into(), Some(StructuredSpec::of::<T>()))
            .await?;
        let failed = |reason: String| Error::Run {
            outcome: Outcome::Failed { reason },
            dispatch_id,
        };
        // The JSON the model sent, which the schema of `finish_structured` checked.
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

    /// Stops the running dispatch and drops every queued one; each settles as
    /// [`Outcome::Aborted`]. With nothing running or queued, does nothing.
    pub async fn abort(&self) -> Result<()> {
        self.actor_ask(|respond_to| Command::Abort { respond_to })
            .await
    }

    /// Submits `text`, asking for `structured` when given.
    async fn submit(&self, text: String, structured: Option<StructuredSpec>) -> Result<Receipt> {
        let dispatch_id = DispatchId::fresh();
        self.actor_ask(|respond_to| Command::Dispatch {
            dispatch_id,
            text,
            structured,
            data: Arc::clone(&self.data),
            respond_to,
        })
        .await?;
        Ok(Receipt { dispatch_id })
    }

    /// Waits for `dispatch_id` to settle and takes what it settled with.
    async fn settled(&self, dispatch_id: DispatchId) -> Result<SettledValue> {
        self.actor_ask(|respond_to| Command::Read {
            dispatch_id,
            respond_to,
        })
        .await
    }

    /// Sends the command `make` builds and waits for its answer;
    /// [`Error::InstanceUnavailable`] when no actor answers.
    async fn actor_ask<R>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<R>>) -> Command,
    ) -> Result<R> {
        self.r.ask(make).await.unwrap_or_else(|| {
            let (agent, id) = self.key.clone();
            Err(Error::InstanceUnavailable { agent, id })
        })
    }
}

/// What an [`AgentInstance`] sends its actor, in order; and what the actor
/// and its run send to themselves.
enum Command {
    Dispatch {
        dispatch_id: DispatchId,
        text: String,
        /// The structured result the message asks for, if any.
        structured: Option<StructuredSpec>,
        /// The handle's instance data, checked against the instance's.
        data: Arc<Value>,
        /// Answered once the dispatch is stored.
        respond_to: oneshot::Sender<Result<()>>,
    },
    Read {
        dispatch_id: DispatchId,
        respond_to: oneshot::Sender<Result<SettledValue>>,
    },
    /// Cancels the running dispatch and settles every queued one as aborted;
    /// answered once stored.
    Abort {
        respond_to: oneshot::Sender<Result<()>>,
    },
    /// A running dispatch's events (B, C), sent by its run; answered once
    /// stored.
    Persist {
        dispatch: DispatchId,
        events: Vec<Event>,
        respond_to: oneshot::Sender<()>,
    },
    /// The run completed; piped, so dropping the actor's `running` drops it.
    Finished {
        dispatch: DispatchId,
        completed: RunCompleted,
    },
    /// The lease is due for renewal: the actor scheduled it for itself.
    Renew,
}

/// What a dispatched message settled with when it produced a reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SettledValue {
    /// The final assistant text.
    pub(crate) text: String,
    /// The structured result, when the message asked for one.
    pub(crate) structured: Option<Value>,
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
