//! Agent instances: a cloneable [`AgentInstance`] handle and the actor behind it
//! (Alice Ryhl's "Actors with Tokio").
//!
//! The actor is the only writer of its instance's event stream, while it
//! holds the stream's lease. It stores each dispatch before returning its
//! receipt, runs dispatches one at a time, step by step, storing what each
//! step produced before the next, and answers reads from what the stream
//! adds up to ([`fold::Folded`]).
//!
//! This file holds the handle; `actor.rs` the actor and its lease;
//! `fold.rs` the fold.

mod actor;
mod fold;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use self::actor::{Control, InstanceActor};
use crate::agent::TurnSetup;
use crate::builtin::structured::StructuredSpec;
use crate::store::Store;
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// One conversation with an agent, addressed by an id.
///
/// A handle to the instance's actor: clones reach the same instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    id: InstanceId,
    /// The actor's inbox.
    inbox: mpsc::Sender<Command>,
    /// The actor's control channel, served before the inbox: aborts.
    control: mpsc::Sender<Control>,
}

impl AgentInstance {
    /// Spawns the actor for instance `id` of the agent named `name`, its turns
    /// set up by `setup`, created with `data` (as JSON) and kept in `store`;
    /// `shutdown` cancelling stops it.
    pub(crate) fn spawn(
        name: String,
        setup: TurnSetup,
        id: InstanceId,
        data: Value,
        store: Store,
        shutdown: &CancellationToken,
    ) -> Self {
        let (inbox, control) =
            InstanceActor::spawn(name, setup, id.clone(), data, store, shutdown.child_token());
        Self { id, inbox, control }
    }

    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &InstanceId {
        &self.id
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
        let (respond_to, response) = oneshot::channel();
        let _ = self.control.send(respond_to).await;
        // An actor that is gone, or stops before answering, has nothing left to abort.
        response.await.unwrap_or(Ok(()))
    }

    /// Submits `text`, asking for `structured` when given.
    async fn submit(&self, text: String, structured: Option<StructuredSpec>) -> Result<Receipt> {
        let dispatch_id = DispatchId::fresh();
        let (respond_to, response) = oneshot::channel();
        let command = Command::Dispatch {
            dispatch_id,
            text,
            structured,
            respond_to,
        };
        // A gone actor drops `respond_to` with the command: aborted, as below.
        let _ = self.inbox.send(command).await;
        response
            .await
            .unwrap_or_else(|_| Err(aborted(dispatch_id)))?;
        Ok(Receipt { dispatch_id })
    }

    /// Waits for `dispatch_id` to settle and takes what it settled with.
    async fn settled(&self, dispatch_id: DispatchId) -> Result<SettledValue> {
        let (respond_to, response) = oneshot::channel();
        let command = Command::Read {
            dispatch_id,
            respond_to,
        };
        let _ = self.inbox.send(command).await;
        // An actor that is gone, or stops before answering (it panicked), counts as aborted.
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
        structured: Option<StructuredSpec>,
        /// Answered once the dispatch is stored.
        respond_to: oneshot::Sender<Result<()>>,
    },
    Read {
        dispatch_id: DispatchId,
        respond_to: oneshot::Sender<Result<SettledValue>>,
    },
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

#[cfg(test)]
mod tests;
