//! Agent instances: a cloneable [`AgentInstance`] handle and the actor behind it
//! (Alice Ryhl's "Actors with Tokio").
//!
//! The actor is the only writer of its instance's event stream, while it
//! holds the stream's lease. It stores each dispatch before returning its
//! receipt, runs dispatches one at a time, step by step, storing what each
//! step produced before the next, and answers reads from what the stream
//! adds up to ([`fold::Folded`]).
//!
//! This file holds the handle; `actor.rs` the actor; `fold.rs` the fold.
//! The handle names the instance by key ([`EntityRef`]): after its actor
//! ends, the next call starts a new one.

mod actor;
mod fold;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(crate) use self::actor::Control;
use self::actor::Start;
use crate::actors::entity::{Entities, EntityRef};
use crate::actors::{self, AskError, Handle};
use crate::agent::TurnSetup;
use crate::agent::rig::Connect;
use crate::builtin::structured::StructuredSpec;
use crate::store::Store;
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// Names an instance by its agent's name and its id.
pub(crate) type InstanceKey = (String, InstanceId);

/// A reference to an instance's actor, by key.
type InstanceRef = EntityRef<InstanceKey, Command, Control>;

/// Every instance's actor, by key.
pub(crate) type Instances = Entities<InstanceKey, Command, Control>;

/// Spawns the actor for instance `key` on `rt`, its turns set up by `setup`
/// and its model connected by `connect`, created with `data` (as JSON) and
/// kept in `store`; `shutdown` cancelling stops it.
pub(crate) fn spawn(
    rt: &tokio::runtime::Handle,
    shutdown: &CancellationToken,
    (name, id): &InstanceKey,
    setup: TurnSetup,
    data: Value,
    connect: Connect,
    store: Store,
) -> Handle<Command, Control> {
    let start = Start {
        name: name.clone(),
        id: id.clone(),
        setup,
        data,
        connect,
        store,
    };
    actors::spawn(rt, shutdown.child_token(), |mailbox| {
        actor::run(start, mailbox)
    })
}

/// One conversation with an agent, addressed by an id.
///
/// A handle to the instance's actor: clones reach the same instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    r: InstanceRef,
}

impl AgentInstance {
    /// The handle to the instance `r` names.
    pub(crate) fn new(r: InstanceRef) -> Self {
        Self { r }
    }

    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &InstanceId {
        &self.r.key().1
    }

    /// Submits `message`; returns a receipt, not the reply.
    ///
    /// The message is stored before the receipt returns, and runs after those
    /// dispatched before it. A store error, or another runtime holding the
    /// instance, shows up here; a model or connect error on [`read`](Self::read).
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
    ///
    /// Served before dispatches and reads already sent.
    pub async fn abort(&self) -> Result<()> {
        self.r
            .control(std::convert::identity)
            .await
            .map_err(|e| self.unavailable(e))?
    }

    /// Submits `text`, asking for `structured` when given.
    async fn submit(&self, text: String, structured: Option<StructuredSpec>) -> Result<Receipt> {
        let dispatch_id = DispatchId::fresh();
        self.r
            .ask(|respond_to| Command::Dispatch {
                dispatch_id,
                text,
                structured,
                respond_to,
            })
            .await
            .map_err(|e| self.unavailable(e))??;
        Ok(Receipt { dispatch_id })
    }

    /// Waits for `dispatch_id` to settle and takes what it settled with.
    async fn settled(&self, dispatch_id: DispatchId) -> Result<SettledValue> {
        self.r
            .ask(|respond_to| Command::Read {
                dispatch_id,
                respond_to,
            })
            .await
            .map_err(|e| self.unavailable(e))?
    }

    /// No actor answered: the runtime was dropped, or the actor ended.
    fn unavailable<M>(&self, _: AskError<M>) -> Error {
        let (agent, id) = self.r.key().clone();
        Error::InstanceUnavailable { agent, id }
    }
}

/// What an [`AgentInstance`] sends its actor, in order.
#[derive(Debug)]
pub(crate) enum Command {
    Dispatch {
        dispatch_id: DispatchId,
        text: String,
        /// The structured result the message asks for, if any.
        structured: Option<StructuredSpec>,
        /// Answered once the dispatch is stored.
        respond_to: oneshot::Sender<Result<()>>,
    },
    Read {
        dispatch_id: DispatchId,
        respond_to: oneshot::Sender<Result<SettledValue>>,
    },
}

impl Command {
    /// Answers the command with `error`.
    fn fail(self, error: Error) {
        let _ = match self {
            Self::Dispatch { respond_to, .. } => respond_to.send(Err(error)).map_err(drop),
            Self::Read { respond_to, .. } => respond_to.send(Err(error)).map_err(drop),
        };
    }
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
