//! The contract every [`StoreBackend`](super::StoreBackend) keeps: run
//! [`contract`] against a [`Store`] over yours.

use std::time::Duration;

use super::{Result, Seq, Store, StoreError};
use crate::event::Event;
use crate::{DispatchId, InstanceId};

/// The lease length the checks use; they wait out one lease, so it is short.
const TTL: Duration = Duration::from_millis(200);

const AGENT: &str = "contract";

/// Checks `store`'s backend against the store contract, panicking on the
/// first broken rule. Each check uses a fresh stream, so the store may hold
/// other data; it waits out a few short leases on the real clock.
pub async fn contract(store: &Store) {
    let store = store.clone().lease_ttl(TTL);
    a_live_lease_is_held(&store).await;
    an_expired_lease_is_taken_over(&store).await;
    a_released_lease_is_free_at_once_and_a_stale_release_is_a_no_op(&store).await;
    a_taken_over_writer_is_fenced(&store).await;
    an_append_at_the_wrong_head_stores_nothing(&store).await;
    read_returns_the_events_after_a_seq_in_order(&store).await;
}

/// An event that tells its stream apart by `n`.
fn event(n: u64) -> (Option<DispatchId>, Event) {
    let data = serde_json::json!({ "n": n });
    (None, Event::Created { data })
}

fn is_held<T>(result: &Result<T>) -> bool {
    matches!(result, Err(StoreError::Held { .. }))
}

fn is_fenced<T>(result: &Result<T>) -> bool {
    matches!(result, Err(StoreError::Fenced { .. }))
}

async fn seqs(store: &Store, id: &InstanceId, after: u64) -> Vec<u64> {
    store
        .read(AGENT, id, Seq::new(after))
        .await
        .expect("read")
        .iter()
        .map(|e| e.seq.get())
        .collect()
}

async fn a_live_lease_is_held(store: &Store) {
    let id = InstanceId::fresh();
    store.acquire(AGENT, &id).await.expect("first acquire");
    let second = store.acquire(AGENT, &id).await;
    assert!(is_held(&second), "a second acquire got {second:?}");
}

async fn an_expired_lease_is_taken_over(store: &Store) {
    let id = InstanceId::fresh();
    let first = store.acquire(AGENT, &id).await.expect("first acquire");
    tokio::time::sleep(TTL * 2).await;
    let second = store
        .acquire(AGENT, &id)
        .await
        .expect("acquire after expiry");
    assert!(second > first, "epoch {second:?} after {first:?}");
}

async fn a_released_lease_is_free_at_once_and_a_stale_release_is_a_no_op(store: &Store) {
    let id = InstanceId::fresh();
    let first = store.acquire(AGENT, &id).await.expect("first acquire");
    store.release(AGENT, &id, first).await.expect("release");
    let second = store
        .acquire(AGENT, &id)
        .await
        .expect("acquire after release");
    assert!(second > first, "epoch {second:?} after {first:?}");
    store
        .release(AGENT, &id, first)
        .await
        .expect("stale release");
    let third = store.acquire(AGENT, &id).await;
    assert!(
        is_held(&third),
        "after a stale release, acquire got {third:?}"
    );
}

async fn a_taken_over_writer_is_fenced(store: &Store) {
    let id = InstanceId::fresh();
    let old = store.acquire(AGENT, &id).await.expect("first acquire");
    store
        .append(AGENT, &id, old, Seq::default(), &[event(1)])
        .await
        .expect("append while holding");
    tokio::time::sleep(TTL * 2).await;
    let new = store.acquire(AGENT, &id).await.expect("takeover");
    let renewed = store.renew(AGENT, &id, old).await;
    assert!(
        is_fenced(&renewed),
        "the old writer's renew got {renewed:?}"
    );
    let appended = store
        .append(AGENT, &id, old, Seq::new(1), &[event(2)])
        .await;
    assert!(
        is_fenced(&appended),
        "the old writer's append got {appended:?}"
    );
    store
        .renew(AGENT, &id, new)
        .await
        .expect("the new writer renews");
    store
        .append(AGENT, &id, new, Seq::new(1), &[event(2)])
        .await
        .expect("the new writer appends");
    assert_eq!(seqs(store, &id, 0).await, [1, 2]);
}

async fn an_append_at_the_wrong_head_stores_nothing(store: &Store) {
    let id = InstanceId::fresh();
    let epoch = store.acquire(AGENT, &id).await.expect("acquire");
    store
        .append(AGENT, &id, epoch, Seq::default(), &[event(1), event(2)])
        .await
        .expect("append");
    // Expects head 1, but it is 2: neither event may land.
    let stale = store
        .append(AGENT, &id, epoch, Seq::new(1), &[event(2), event(3)])
        .await;
    assert!(is_fenced(&stale), "an append at a stale head got {stale:?}");
    assert_eq!(seqs(store, &id, 0).await, [1, 2]);
}

async fn read_returns_the_events_after_a_seq_in_order(store: &Store) {
    let id = InstanceId::fresh();
    assert!(
        seqs(store, &id, 0).await.is_empty(),
        "a new stream is empty"
    );
    let epoch = store.acquire(AGENT, &id).await.expect("acquire");
    let events = [event(1), event(2)];
    store
        .append(AGENT, &id, epoch, Seq::default(), &events)
        .await
        .expect("append");
    store
        .append(AGENT, &id, epoch, Seq::new(2), &[event(3)])
        .await
        .expect("append");
    assert_eq!(seqs(store, &id, 1).await, [2, 3]);
    let read = store.read(AGENT, &id, Seq::default()).await.expect("read");
    let bodies: Vec<_> = read[..2]
        .iter()
        .map(|e| (e.kind.clone(), e.event.clone()))
        .collect();
    let sent: Vec<_> = events.iter().map(|(_, e)| e.to_parts()).collect();
    assert_eq!(bodies, sent, "events come back as they were stored");
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn the_in_memory_store_keeps_the_contract() {
        super::contract(&super::Store::in_memory()).await;
    }
}
