//! Keyed actors: [`Entities`] holds every actor of one type by key, as an
//! Akka shard region does for one entity type. A key's task is spawned once
//! and kept: its [`ActorRef`] never changes.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::{ActorRef, Mailbox};

/// The actors of one type by key, each with the start data `V` it was
/// spawned with.
pub(crate) struct Entities<Key, C, V> {
    refs: Mutex<HashMap<Key, (ActorRef<C>, V)>>,
    rt: tokio::runtime::Handle,
    /// Parent of every actor's token: cancelling it stops them all.
    cancel: CancellationToken,
}

impl<Key: Clone + Eq + Hash, C: Send + 'static, V> Entities<Key, C, V> {
    /// No actors yet; each is spawned on `rt` and stopped when `cancel` is.
    pub(crate) fn new(rt: tokio::runtime::Handle, cancel: CancellationToken) -> Self {
        Self {
            refs: Mutex::default(),
            rt,
            cancel,
        }
    }

    /// The key's actor: spawned once, running `run`, and kept with its start
    /// data `init`; when it exists, `check(kept, &init)` decides.
    pub(crate) fn get<E, F>(
        &self,
        key: Key,
        init: V,
        check: impl FnOnce(&V, &V) -> Result<(), E>,
        run: impl FnOnce(Mailbox<C>) -> F,
    ) -> Result<ActorRef<C>, E>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut refs = self.refs.lock();
        if let Some((r, kept)) = refs.get(&key) {
            check(kept, &init)?;
            return Ok(r.clone());
        }
        let r = super::spawn(&self.rt, self.cancel.child_token(), run);
        refs.insert(key, (r.clone(), init));
        Ok(r)
    }
}
