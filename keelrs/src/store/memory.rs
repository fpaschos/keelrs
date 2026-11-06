//! The in-memory store: every stream in one map, behind one lock held without `.await`.

use std::collections::HashMap;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

use super::{Epoch, Seq, StoreBackend, StoreError, StoredEvent};
use crate::{InstanceId, Result};

/// Streams keyed by `(agent, instance)`.
#[derive(Debug, Default)]
pub(super) struct Memory(Mutex<HashMap<(String, InstanceId), Stream>>);

#[derive(Debug, Default)]
struct Stream {
    /// The current lease's epoch; 0 before the first.
    epoch: u64,
    /// When the current lease runs out; `None` once released or never taken.
    expires: Option<Instant>,
    events: Vec<StoredEvent>,
}

impl Stream {
    fn head(&self) -> Seq {
        self.events.last().map_or_else(Seq::default, |e| e.seq)
    }

    fn is_current(&self, epoch: Epoch) -> bool {
        self.epoch == epoch.get() && self.expires.is_some()
    }
}

fn key(agent: &str, instance: &InstanceId) -> (String, InstanceId) {
    (agent.to_owned(), instance.clone())
}

fn fenced(agent: &str, instance: &InstanceId) -> crate::Error {
    StoreError::Fenced {
        agent: agent.to_owned(),
        id: instance.clone(),
    }
    .into()
}

#[async_trait::async_trait]
impl StoreBackend for Memory {
    async fn acquire(&self, agent: &str, instance: &InstanceId, ttl: Duration) -> Result<Epoch> {
        let now = Instant::now();
        let mut streams = self.0.lock();
        let stream = streams.entry(key(agent, instance)).or_default();
        if stream.expires.is_some_and(|expires| expires > now) {
            return Err(StoreError::Held {
                agent: agent.to_owned(),
                id: instance.clone(),
            }
            .into());
        }
        stream.epoch += 1;
        stream.expires = Some(now + ttl);
        Ok(Epoch::new(stream.epoch))
    }

    async fn renew(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
        ttl: Duration,
    ) -> Result<()> {
        let mut streams = self.0.lock();
        match streams.get_mut(&key(agent, instance)) {
            Some(stream) if stream.is_current(epoch) => {
                stream.expires = Some(Instant::now() + ttl);
                Ok(())
            }
            _ => Err(fenced(agent, instance)),
        }
    }

    async fn release(&self, agent: &str, instance: &InstanceId, epoch: Epoch) -> Result<()> {
        if let Some(stream) = self.0.lock().get_mut(&key(agent, instance))
            && stream.epoch == epoch.get()
        {
            stream.expires = None;
        }
        Ok(())
    }

    async fn read(
        &self,
        agent: &str,
        instance: &InstanceId,
        after: Seq,
    ) -> Result<Vec<StoredEvent>> {
        let streams = self.0.lock();
        Ok(streams
            .get(&key(agent, instance))
            .map(|stream| {
                stream
                    .events
                    .iter()
                    .filter(|e| e.seq > after)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn append(
        &self,
        agent: &str,
        instance: &InstanceId,
        epoch: Epoch,
        expected: Seq,
        events: &[StoredEvent],
    ) -> Result<()> {
        let mut streams = self.0.lock();
        match streams.get_mut(&key(agent, instance)) {
            // Only the epoch fences, as in SQL: an expired lease not yet taken over still writes.
            Some(stream) if stream.epoch == epoch.get() && stream.head() == expected => {
                stream.events.extend_from_slice(events);
                Ok(())
            }
            _ => Err(fenced(agent, instance)),
        }
    }
}
