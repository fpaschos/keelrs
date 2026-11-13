//! Keyed actors: [`Entities`] holds every actor of one type by key, as an
//! Akka shard region does for one entity type. A key's task is spawned once
//! and kept: its [`ActorRef`] never changes.

use std::collections::HashMap;
use std::hash::Hash;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::{Actor, ActorRef};

/// The actors of type `A` by key, each with the start data `V` it was
/// spawned with.
pub(crate) struct Entities<Key, A: Actor, V> {
    refs: Mutex<HashMap<Key, (ActorRef<A::Command>, V)>>,
    rt: tokio::runtime::Handle,
    /// Parent of every actor's token: cancelling it stops them all.
    cancel: CancellationToken,
}

impl<Key: Clone + Eq + Hash, A: Actor, V> Entities<Key, A, V> {
    /// No actors yet; each is spawned on `rt` and stopped when `cancel` is.
    pub(crate) fn new(rt: tokio::runtime::Handle, cancel: CancellationToken) -> Self {
        Self {
            refs: Mutex::default(),
            rt,
            cancel,
        }
    }

    /// The key's actor: spawned once, built from `args` at each start, and
    /// kept with its start data `init`; when it exists, `check(kept, &init)`
    /// decides and `args` is dropped.
    pub(crate) fn get<E>(
        &self,
        key: Key,
        init: V,
        check: impl FnOnce(&V, &V) -> Result<(), E>,
        args: A::Args,
    ) -> Result<ActorRef<A::Command>, E> {
        let mut refs = self.refs.lock();
        if let Some((r, kept)) = refs.get(&key) {
            check(kept, &init)?;
            return Ok(r.clone());
        }
        let r = super::spawn::<A>(&self.rt, self.cancel.child_token(), args);
        refs.insert(key, (r.clone(), init));
        Ok(r)
    }
}
