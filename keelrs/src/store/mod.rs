//! Where an agent instance's events are kept.
//!
//! A [`Store`] is a cheap, cloneable handle over a [`StoreBackend`]: memory
//! ([`Store::in_memory`]), or later a database. Each agent instance has one
//! event stream, named by its agent and instance id, that only the runtime
//! holding its [lease](StoreBackend::acquire) writes.
//!
//! What a backend guarantees, and what it does not:
//!
//! - An append stores all its events or none, and only while the writer's
//!   epoch and the stream's head still match: a writer that lost its lease
//!   is fenced, never interleaved with the next one.
//! - The lease decides who may write; integrity rests only on the epoch and
//!   head check at append time, on the store's own clock.
//! - No exactly-once external effects: a tool run by a writer that lost its
//!   lease may have happened without its result being stored.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::{DispatchId, InstanceId, Result};

mod memory;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// How long a lease lasts unless renewed: 30 s.
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);

/// The name of one event: unique everywhere, minted by the writer before the append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(uuid::Uuid);

impl EventId {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An event's position in its stream: 1, 2, 3… with no gaps; 0 before the first.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    /// The position `n`; [`Seq::default`] is 0, before the first event.
    #[must_use]
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    /// The position as a number.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// The position after this one.
    #[must_use]
    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Which holder of a stream's lease this is: each [`acquire`](StoreBackend::acquire)
/// returns a larger one, and a write with an older one is fenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(u64);

impl Epoch {
    /// The epoch `n`.
    #[must_use]
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    /// The epoch as a number.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// One event as its stream holds it: the envelope, with the event as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    /// The event's name.
    pub id: EventId,
    /// The agent whose instance's stream holds it.
    pub agent: String,
    /// The instance whose stream holds it.
    pub instance: InstanceId,
    /// Its position in the stream.
    pub seq: Seq,
    /// The dispatch that caused it; `None` outside a dispatch.
    pub dispatch: Option<DispatchId>,
    /// When the writer made it, ms since the Unix epoch.
    pub at: u64,
    /// What kind of event it is, such as `settled`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The event's fields, as JSON; their shape depends on `kind`.
    pub event: serde_json::Value,
}

/// Something a store could not do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Another runtime holds the stream's lease.
    #[error("the events of instance `{id}` of agent `{agent}` are held by another runtime")]
    Held {
        /// The agent's name.
        agent: String,
        /// The instance's id.
        id: InstanceId,
    },
    /// The writer's lease was taken over, or the stream moved on without it.
    #[error("the writer of instance `{id}` of agent `{agent}` lost its lease")]
    Fenced {
        /// The agent's name.
        agent: String,
        /// The instance's id.
        id: InstanceId,
    },
    /// The store failed, or refused malformed events.
    #[error("store failed: {reason}")]
    Failed {
        /// What went wrong, as plain text.
        reason: String,
    },
}

/// A place to keep event streams: what a [`Store`] calls.
///
/// Each method is atomic. Check an implementation with
/// [`testing::contract`] (the `testing` feature).
#[async_trait::async_trait]
pub trait StoreBackend: Send + Sync + 'static {
    /// Takes the stream's lease for `ttl` and returns its new epoch;
    /// [`StoreError::Held`] while another lease is live.
    async fn acquire(&self, agent: &str, instance: &InstanceId, ttl: Duration) -> Result<Epoch>;

    /// Extends the lease to `ttl` from now; [`StoreError::Fenced`] if `epoch`
    /// is no longer current.
    async fn renew(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
        ttl: Duration,
    ) -> Result<()>;

    /// Ends the lease now; a stale `epoch` is a no-op.
    async fn release(&self, agent: &str, instance: &InstanceId, epoch: Epoch) -> Result<()>;

    /// The stream's events after `after`, in seq order.
    async fn read(
        &self,
        agent: &str,
        instance: &InstanceId,
        after: Seq,
    ) -> Result<Vec<StoredEvent>>;

    /// Stores `events` (numbered `expected`+1…) all or none, only while
    /// `epoch` is current and the stream's last seq is `expected`; otherwise
    /// [`StoreError::Fenced`].
    async fn append(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
        expected: Seq,
        events: &[StoredEvent],
    ) -> Result<()>;
}

/// Where a runtime keeps its agent instances' events: a cheap, cloneable
/// handle over a [`StoreBackend`], with the lease length its writers use.
#[derive(Clone)]
pub struct Store {
    backend: Arc<dyn StoreBackend>,
    ttl: Duration,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Store {
    /// A store over `backend`, with 30 s leases.
    pub fn new(backend: impl StoreBackend) -> Self {
        Self {
            backend: Arc::new(backend),
            ttl: DEFAULT_LEASE_TTL,
        }
    }

    /// A store in this process's memory: its events go when the last clone does.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(memory::Memory::default())
    }

    /// How long a lease lasts unless renewed; a writer renews it every third
    /// of this, and stops running tools once it may have run out.
    #[must_use]
    pub fn lease_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub(crate) fn ttl(&self) -> Duration {
        self.ttl
    }

    pub(crate) async fn acquire(&self, agent: &str, instance: &InstanceId) -> Result<Epoch> {
        self.backend.acquire(agent, instance, self.ttl).await
    }

    pub(crate) async fn renew(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
    ) -> Result<()> {
        self.backend.renew(agent, instance, epoch, self.ttl).await
    }

    pub(crate) async fn release(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
    ) -> Result<()> {
        self.backend.release(agent, instance, epoch).await
    }

    pub(crate) async fn read(
        &self,
        agent: &str,
        instance: &InstanceId,
        after: Seq,
    ) -> Result<Vec<StoredEvent>> {
        self.backend.read(agent, instance, after).await
    }

    /// Appends `events`, each with the dispatch that caused it, numbered
    /// `expected`+1… in the stream of instance `instance` of `agent`.
    pub(crate) async fn append(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
        expected: Seq,
        events: &[(Option<DispatchId>, Event)],
    ) -> Result<()> {
        let at = now_ms();
        let mut seq = expected;
        let stored: Vec<StoredEvent> = events
            .iter()
            .map(|(dispatch, event)| {
                seq = seq.next();
                let (kind, event) = event.to_parts();
                StoredEvent {
                    id: EventId::fresh(),
                    agent: agent.to_owned(),
                    instance: instance.clone(),
                    seq,
                    dispatch: *dispatch,
                    at,
                    kind,
                    event,
                }
            })
            .collect();
        self.backend
            .append(agent, instance, epoch, expected, &stored)
            .await
    }
}

/// Now, in ms since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}
