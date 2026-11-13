//! An agent instance's events kept in a shared `Store`: recovery by a later
//! runtime, the lease between two runtimes, the instance's data and abort.
//! Most runs fail fast (no model is declared).

use keelrs::store::{Store, StoreError};
use keelrs::testing::ScriptedModel;
use keelrs::{Agent, AgentContext, Error, InstanceId, Receipt, Runtime};
use serde_json::json;
use tokio::time::timeout;

use crate::support::{
    TIMEOUT, agent, clock, is_aborted, is_failed, once_free, runtime_on, until_requests,
};

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
    let receipt = once_free(|| other.dispatch("Me too")).await.unwrap();
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
    let conflict = once_free(|| instance.dispatch("Hello")).await;
    assert!(
        matches!(conflict, Err(Error::InstanceDataConflict { .. })),
        "got {conflict:?}"
    );
    // Calls queued while a new incarnation starts get the conflict too.
    let (a, b) = tokio::join!(instance.dispatch("A"), instance.dispatch("B"));
    for queued in [a, b] {
        assert!(
            matches!(queued, Err(Error::InstanceDataConflict { .. })),
            "got {queued:?}"
        );
    }
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

/// A stream a dropped runtime left with two dispatches: the first stopped in
/// its second model call, after one stored tool round; the second never
/// started. And their receipts.
async fn left_interrupted(store: &Store, agent: &Agent, id: &InstanceId) -> [Receipt; 2] {
    let model = ScriptedModel::new()
        .calls([("c1", "current_time", json!({}))])
        .hang();
    let runtime = runtime_on(store, agent, &model);
    let instance = runtime.instance(agent, id.clone()).unwrap();
    let receipts = [
        instance.dispatch("One").await.unwrap(),
        instance.dispatch("Two").await.unwrap(),
    ];
    // The first dispatch calls the model again.
    until_requests(&model, 2).await;
    receipts
}

#[tokio::test]
async fn a_read_that_recovers_the_stream_aborts_the_interrupted_dispatch_and_runs_the_queued_one() {
    let (store, id) = (Store::in_memory(), InstanceId::fresh());
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let [first, second] = left_interrupted(&store, &agent, &id).await;

    let model = ScriptedModel::new().reply("Two done.");
    let runtime = runtime_on(&store, &agent, &model);
    let instance = runtime.instance(&agent, id).unwrap();
    let read = timeout(TIMEOUT, once_free(|| instance.read(&second))).await;
    assert_eq!(read.unwrap().unwrap().text(), "Two done.");
    assert!(is_aborted(&instance.read(&first).await));
}

#[tokio::test]
async fn abort_settles_dispatches_it_has_not_recovered_yet() {
    let (store, id) = (Store::in_memory(), InstanceId::fresh());
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let receipts = left_interrupted(&store, &agent, &id).await;

    let model = ScriptedModel::new();
    let runtime = runtime_on(&store, &agent, &model);
    let instance = runtime.instance(&agent, id).unwrap();
    once_free(|| instance.abort()).await.unwrap();
    for receipt in &receipts {
        let read = timeout(TIMEOUT, instance.read(receipt)).await.unwrap();
        assert!(is_aborted(&read), "got {read:?}");
    }
    assert!(model.requests().is_empty(), "nothing ran");
}
