//! keelrs's canonical events: the source of truth for an agent instance (ADR 0004).
//!
//! Private and minimal for now: just enough to rebuild the history for the next
//! dispatch and to answer reads, once durable storage keeps them (`STORE:`).

use serde::{Deserialize, Serialize};

use crate::agent::SettledValue;
use crate::agent::rig::Message;
use crate::{DispatchId, InstanceId, Outcome};

/// The name of one event: unique everywhere, minted by the writer before the append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct EventId(uuid::Uuid);

impl EventId {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

/// An event's position in its stream: 1, 2, 3… with no gaps; 0 before the first.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub(crate) struct Seq(u64);

impl Seq {
    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// One event as its stream holds it.
#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "STORE: read back once durable storage keeps events"
)]
pub(crate) struct StoredEvent {
    pub(crate) id: EventId,
    pub(crate) agent: String,
    pub(crate) instance: InstanceId,
    pub(crate) seq: Seq,
    /// The dispatch that caused it; `None` outside a dispatch.
    pub(crate) dispatch: Option<DispatchId>,
    /// When the writer made it, ms since the Unix epoch.
    pub(crate) at: u64,
    pub(crate) event: Event,
}

#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "STORE: read back once durable storage keeps events"
)]
pub(crate) enum Event {
    /// A message the run accepted (the user's prompt or the assistant's turn).
    Message(Message),
    /// The dispatched message settled: its reply text and structured result, or how it failed.
    Settled {
        dispatch_id: DispatchId,
        result: Result<SettledValue, Outcome>,
    },
}
