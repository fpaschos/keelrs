//! The single writer: a lease on a stream, taken from the store, renewed on a
//! deadline, trusted only while it cannot have run out.

use std::time::Duration;

use tokio::time::Instant;

use crate::InstanceId;
use crate::store::{Epoch, Store, StoreError};

/// A held lease on the event stream of instance `id` of `agent`.
pub(crate) struct Lease {
    store: Store,
    agent: String,
    id: InstanceId,
    epoch: Epoch,
    /// When the lease was last asked for or renewed (monotonic).
    renewed: Instant,
}

impl Lease {
    /// Takes the lease; [`StoreError::Held`] while another writer holds it.
    pub(crate) async fn acquire(
        store: Store,
        agent: String,
        id: InstanceId,
    ) -> Result<Self, StoreError> {
        let asked = Instant::now();
        let epoch = store.acquire(&agent, &id).await?;
        Ok(Self {
            store,
            agent,
            id,
            epoch,
            renewed: asked,
        })
    }

    /// Extends the lease; from when it was asked, not when it was granted.
    pub(crate) async fn renew(&mut self) -> Result<(), StoreError> {
        let asked = Instant::now();
        self.store.renew(&self.agent, &self.id, self.epoch).await?;
        self.renewed = asked;
        Ok(())
    }

    /// When to renew: a third of the TTL after the last renewal.
    pub(crate) fn renew_at(&self) -> Instant {
        self.renewed + (self.store.ttl() / 3).max(Duration::from_millis(1))
    }

    /// Whether the lease can still be trusted to start a step: it cannot
    /// have run out, with a tenth of its TTL to spare.
    pub(crate) fn trusted(&self) -> bool {
        let ttl = self.store.ttl();
        Instant::now() < self.renewed + (ttl - ttl / 10)
    }

    pub(crate) fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Gives the lease up, best-effort: unreleased, it runs out by itself.
    pub(crate) async fn release(self) {
        let _ = self.store.release(&self.agent, &self.id, self.epoch).await;
    }
}
