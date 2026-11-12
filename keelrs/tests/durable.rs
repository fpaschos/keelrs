//! An agent instance's events kept in a shared `Store`: recovery by a later
//! runtime, the lease between two runtimes, the instance's data and abort.
//! Runs fail fast (no model is declared), so nothing reaches the network.

use std::time::Duration;

use keelrs::store::{Store, StoreError};
use keelrs::{Agent, AgentContext, AgentInstance, Error, InstanceId, Outcome, Receipt, Runtime};

/// An agent whose every run fails at once: it never calls `use_model`.
fn failing() -> Agent<u32> {
    Agent::new("failing", |_: &mut AgentContext<u32>| "Greet.".into())
}

fn on(store: &Store, agent: &Agent<u32>) -> Runtime {
    Runtime::builder()
        .store(store.clone())
        .start([agent])
        .unwrap()
}

fn is_failed(result: &keelrs::Result<impl std::fmt::Debug>) -> bool {
    matches!(
        result,
        Err(Error::Run {
            outcome: Outcome::Failed { .. },
            ..
        })
    )
}

/// Dispatches `message`, waiting while a dropped runtime still holds the lease.
async fn dispatch_once_free(instance: &AgentInstance, message: &str) -> keelrs::Result<Receipt> {
    for _ in 0..100 {
        match instance.dispatch(message).await {
            Err(Error::Store(StoreError::Held { .. })) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            other => return other,
        }
    }
    panic!("the lease was never released");
}

#[tokio::test]
async fn a_later_runtime_recovers_the_instance_and_continues_its_stream() {
    let store = Store::in_memory();
    let agent = failing();
    let id = InstanceId::new("recovered");

    let first = on(&store, &agent);
    let instance = first.instance_with(&agent, id.clone(), 7).unwrap();
    let earlier = instance.dispatch("Hello").await.unwrap();
    assert!(is_failed(&instance.read(&earlier).await));
    drop(first);

    let second = on(&store, &agent);
    let instance = second.instance_with(&agent, id, 7).unwrap();
    // Its events number on from the first runtime's, or the store would refuse them.
    let later = dispatch_once_free(&instance, "Again").await.unwrap();
    assert!(is_failed(&instance.read(&later).await));
    // The first runtime's dispatch is read from the stream.
    assert!(is_failed(&instance.read(&earlier).await));
}

#[tokio::test]
async fn a_second_runtime_is_held_off_until_the_first_drops() {
    let store = Store::in_memory();
    let agent = failing();
    let id = InstanceId::new("held");

    let first = on(&store, &agent);
    let holder = first.instance_with(&agent, id.clone(), 7).unwrap();
    holder.ask("Hello").await.unwrap_err();

    let second = on(&store, &agent);
    let other = second.instance_with(&agent, id, 7).unwrap();
    let held = other.dispatch("Me too").await;
    assert!(
        matches!(held, Err(Error::Store(StoreError::Held { .. }))),
        "got {held:?}"
    );

    drop(first);
    let receipt = dispatch_once_free(&other, "Me too").await.unwrap();
    assert!(is_failed(&other.read(&receipt).await));
}

#[tokio::test]
async fn other_data_for_a_stored_instance_conflicts_at_dispatch() {
    let store = Store::in_memory();
    let agent = failing();
    let id = InstanceId::new("data");

    let first = on(&store, &agent);
    let instance = first.instance_with(&agent, id.clone(), 7).unwrap();
    instance.ask("Hello").await.unwrap_err();
    drop(first);

    let second = on(&store, &agent);
    let instance = second.instance_with(&agent, id, 8).unwrap();
    let conflict = dispatch_once_free(&instance, "Hello").await;
    assert!(
        matches!(conflict, Err(Error::InstanceDataConflict { .. })),
        "got {conflict:?}"
    );
}

#[tokio::test]
async fn abort_with_nothing_running_does_nothing() {
    let agent = failing();
    let runtime = Runtime::start([&agent]).unwrap();
    let instance = runtime
        .instance_with(&agent, InstanceId::new("idle"), 7)
        .unwrap();
    instance.abort().await.unwrap();
    let receipt = instance.dispatch("Hello").await.unwrap();
    assert!(is_failed(&instance.read(&receipt).await));
    instance.abort().await.unwrap();
}
