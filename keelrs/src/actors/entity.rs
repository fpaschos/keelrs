//! Keyed actors: [`Entities`] holds every actor of one type by key, as an
//! Akka shard region does for one entity type. A key's task is spawned once
//! and kept: its [`ActorRef`] never changes.

use std::collections::HashMap;
use std::hash::Hash;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::{Actor, ActorRef};

/// The actors of type `A` by key.
pub(crate) struct Entities<Key, A: Actor> {
    refs: Mutex<HashMap<Key, ActorRef<A::Command>>>,
    rt: tokio::runtime::Handle,
    /// Parent of every actor's token: cancelling it stops them all.
    cancel: CancellationToken,
}

impl<Key: Eq + Hash, A: Actor> Entities<Key, A> {
    /// No actors yet; each is spawned on `rt` and stopped when `cancel` is.
    pub(crate) fn new(rt: tokio::runtime::Handle, cancel: CancellationToken) -> Self {
        Self {
            refs: Mutex::default(),
            rt,
            cancel,
        }
    }

    /// The key's actor; spawned with `args` when the key is new, otherwise
    /// `args` is dropped.
    pub(crate) fn get(&self, key: Key, args: A::Args) -> ActorRef<A::Command> {
        self.refs
            .lock()
            .entry(key)
            .or_insert_with(|| super::spawn::<A>(&self.rt, self.cancel.child_token(), args))
            .clone()
    }
}
