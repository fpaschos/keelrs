//! Keyed actors: an [`EntityRef`] names an actor by key, as Akka's
//! `EntityRef` or an Orleans grain reference does, not by its running task.
//! A key's actor is spawned on the first call, and spawned again on the call
//! after it ended.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{AskError, Handle};

/// One key's live actor, if any; locked only to read or replace it.
type Slot<C, K> = Arc<Mutex<Option<Handle<C, K>>>>;

/// Every key's slot; locked only to find or add a slot.
type Slots<Key, C, K> = Arc<Mutex<HashMap<Key, Slot<C, K>>>>;

/// Starts a key's actor.
type Spawner<Key, C, K> = Arc<dyn Fn(&Key) -> Handle<C, K> + Send + Sync>;

/// The actors of one kind, by key.
pub(crate) struct Entities<Key, C, K> {
    slots: Slots<Key, C, K>,
    spawn: Spawner<Key, C, K>,
    /// Cancelled when the owner is dropped: no actor is spawned after.
    cancel: CancellationToken,
}

impl<Key, C, K> Clone for Entities<Key, C, K> {
    fn clone(&self) -> Self {
        Self {
            slots: Arc::clone(&self.slots),
            spawn: Arc::clone(&self.spawn),
            cancel: self.cancel.clone(),
        }
    }
}

impl<Key: Clone + Eq + Hash, C, K> Entities<Key, C, K> {
    /// No actors yet; `spawn` starts a key's, until `cancel` is cancelled.
    pub(crate) fn new(
        spawn: impl Fn(&Key) -> Handle<C, K> + Send + Sync + 'static,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            slots: Arc::default(),
            spawn: Arc::new(spawn),
            cancel,
        }
    }

    /// The reference to `key`'s actor. Spawns nothing: the first call does.
    pub(crate) fn get(&self, key: Key) -> EntityRef<Key, C, K> {
        let slot = Arc::clone(self.slots.lock().entry(key.clone()).or_default());
        EntityRef {
            key,
            slot,
            entities: self.clone(),
        }
    }
}

/// Names one actor by key; each call reaches its live incarnation, spawning
/// one when there is none.
pub(crate) struct EntityRef<Key, C, K> {
    key: Key,
    slot: Slot<C, K>,
    entities: Entities<Key, C, K>,
}

impl<Key: Clone, C, K> Clone for EntityRef<Key, C, K> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            slot: Arc::clone(&self.slot),
            entities: self.entities.clone(),
        }
    }
}

impl<Key: fmt::Debug, C, K> fmt::Debug for EntityRef<Key, C, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntityRef")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<Key, C, K> EntityRef<Key, C, K> {
    pub(crate) fn key(&self) -> &Key {
        &self.key
    }

    /// The live actor's handle, spawning one if the slot is empty or its
    /// actor ended; `None` once the owner is dropped.
    fn handle(&self) -> Option<Handle<C, K>> {
        if self.entities.cancel.is_cancelled() {
            return None;
        }
        let mut slot = self.slot.lock();
        if let Some(handle) = slot.as_ref().filter(|handle| !handle.is_closed()) {
            return Some(handle.clone());
        }
        let handle = (self.entities.spawn)(&self.key);
        *slot = Some(handle.clone());
        Some(handle)
    }

    /// Sends the command `make` builds and waits for its answer.
    pub(crate) async fn ask<R>(
        &self,
        make: impl FnOnce(oneshot::Sender<R>) -> C,
    ) -> Result<R, AskError<C>> {
        self.call(make, Handle::inbox).await
    }

    /// Sends the control message `make` builds and waits for its answer.
    pub(crate) async fn control<R>(
        &self,
        make: impl FnOnce(oneshot::Sender<R>) -> K,
    ) -> Result<R, AskError<K>> {
        self.call(make, Handle::control).await
    }

    /// Sends the message `make` builds on the channel `to` picks and waits
    /// for its answer; when the actor ended before taking it, re-attaches
    /// once and sends it again.
    async fn call<M, R>(
        &self,
        make: impl FnOnce(oneshot::Sender<R>) -> M,
        to: fn(&Handle<C, K>) -> &mpsc::Sender<M>,
    ) -> Result<R, AskError<M>> {
        let (respond_to, response) = oneshot::channel();
        let mut message = make(respond_to);
        for _ in 0..2 {
            let Some(handle) = self.handle() else { break };
            match to(&handle).send(message).await {
                Ok(()) => return response.await.map_err(|_| AskError::NoReply),
                Err(mpsc::error::SendError(back)) => message = back,
            }
        }
        Err(AskError::NotDelivered(message))
    }
}
