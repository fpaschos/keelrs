//! What an instance's stream adds up to: the history a later dispatch gets,
//! and what an abort leaves behind.

use std::sync::Arc;

use keelrs::store::Store;
use keelrs::testing::ScriptedModel;
use keelrs::{InstanceId, Runtime, Tool, ToolCall};
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::timeout;

use crate::support::{TIMEOUT, agent, clock, is_aborted, once_free, runtime, until_requests};

#[tokio::test]
async fn a_later_runtimes_model_sees_the_history_the_first_one_stored() {
    let store = Store::in_memory();
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let id = InstanceId::fresh();
    let on = |model: &ScriptedModel| {
        Runtime::builder()
            .store(store.clone())
            .model(model.clone())
            .start([&agent])
            .unwrap()
    };

    let first = ScriptedModel::new()
        .calls([("c1", "current_time", json!({}))])
        .reply("It is noon.");
    let runtime = on(&first);
    let instance = runtime.instance(&agent, id.clone()).unwrap();
    let reply = timeout(TIMEOUT, instance.ask("What time is it?")).await;
    assert_eq!(reply.unwrap().unwrap().text(), "It is noon.");
    drop(runtime);

    let later = ScriptedModel::new().reply("Still noon.");
    let runtime = on(&later);
    let instance = runtime.instance(&agent, id).unwrap();
    let receipt = once_free(|| instance.dispatch("And now?")).await.unwrap();
    timeout(TIMEOUT, instance.read(&receipt))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        later.requests()[0].messages,
        [
            "What time is it?",
            "call:current_time",
            "result:noon",
            "It is noon.",
            "And now?"
        ]
    );
}

#[tokio::test]
async fn abort_stops_a_live_step_and_settles_it_as_aborted() {
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let model = ScriptedModel::new().hang().reply("Noon.");
    let runtime = runtime(&agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();
    let receipt = instance.dispatch("What time is it?").await.unwrap();
    until_requests(&model, 1).await;

    instance.abort().await.unwrap();
    let read = timeout(TIMEOUT, instance.read(&receipt)).await.unwrap();
    assert!(is_aborted(&read), "got {read:?}");
    // The instance takes the next dispatch.
    let reply = timeout(TIMEOUT, instance.ask("And now?")).await.unwrap();
    assert_eq!(reply.unwrap().text(), "Noon.");
}

#[tokio::test]
async fn an_aborted_dispatchs_open_tool_calls_are_answered_before_the_next_one() {
    // A tool that says it started, then never ends.
    let started = Arc::new(Notify::new());
    let agent = agent({
        let started = Arc::clone(&started);
        move |cx| {
            let started = Arc::clone(&started);
            cx.use_tool(Tool::host(
                "wait",
                "Waits forever.",
                async move |_: ToolCall<()>| {
                    started.notify_one();
                    std::future::pending::<Result<(), std::convert::Infallible>>().await
                },
            ));
        }
    })
    .await;
    let model = ScriptedModel::new()
        .calls([("c1", "wait", json!({}))])
        .reply("Noon.");
    let runtime = runtime(&agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();
    let aborted = instance.dispatch("What time is it?").await.unwrap();
    timeout(TIMEOUT, started.notified()).await.unwrap();

    instance.abort().await.unwrap();
    let read = timeout(TIMEOUT, instance.read(&aborted)).await.unwrap();
    assert!(is_aborted(&read), "got {read:?}");
    timeout(TIMEOUT, instance.ask("Try again."))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        model.requests()[1].messages,
        [
            "What time is it?",
            "call:wait",
            "error:aborted",
            "Try again."
        ]
    );
}
