//! The tool loop: results reaching the model, concurrency, the turn cap, tool order.

use std::convert::Infallible;
use std::sync::Arc;

use keelrs::sandbox::Sandbox;
use keelrs::testing::ScriptedModel;
use keelrs::{Tool, ToolCall};
use serde_json::{Value, json};
use tokio::sync::Barrier;

use crate::support::{
    TempDir, agent, clock, last_results, lookup_order, open, reason, sandboxed_agent, scripted,
    tool_names,
};

#[tokio::test]
async fn a_tool_result_reaches_the_model_and_the_run_settles() {
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let model = ScriptedModel::new()
        .calls([("c1", "current_time", json!({}))])
        .reply("It is noon.");
    let reply = scripted(&agent, &model, "Go").await;
    assert_eq!(reply.unwrap(), "It is noon.");
    assert_eq!(
        model.requests()[1].messages,
        ["Go", "call:current_time", "result:noon"]
    );
}

#[tokio::test]
async fn a_sandboxed_tool_gets_the_sandbox_its_turn_declared() {
    let dir = TempDir::new();
    let declared = open(&dir).await.work_dir("keelrs-declared");
    let expected = json!({"id": declared.id().to_string(), "dir": declared.dir().to_string()});
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&declared);
        cx.use_tool(Tool::sandbox(
            "where_am_i",
            "Where the tool runs.",
            async |_: ToolCall<()>, sb: Sandbox| {
                Ok::<_, Infallible>(json!({"id": sb.id().to_string(), "dir": sb.dir().to_string()}))
            },
        ));
    });
    let results = one_call(&agent, "where_am_i", json!({})).await;
    assert_eq!(results, [format!("result:{expected}")]);
}

/// One tool call, then the reply "Done.", and the results the model got.
async fn one_call(agent: &keelrs::Agent, name: &str, arguments: Value) -> Vec<String> {
    let model = ScriptedModel::new()
        .calls([("c1", name, arguments)])
        .reply("Done.");
    scripted(agent, &model, "Go").await.unwrap();
    last_results(&model)
}

#[tokio::test]
async fn a_call_to_an_undeclared_tool_names_the_available_ones() {
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let results = one_call(&agent, "issue_refund", json!({})).await;
    assert_eq!(
        results,
        ["error:tool \"issue_refund\" is not available; available tools: current_time"]
    );
}

#[tokio::test]
async fn one_replys_calls_run_concurrently_and_are_committed_in_call_order() {
    // Each call waits for the other: run one after another, they never finish.
    let barrier = Arc::new(Barrier::new(2));
    let agent = agent(move |cx| {
        for name in ["first", "second"] {
            let barrier = barrier.clone();
            cx.use_tool(Tool::host(
                name,
                "Waits for the other.",
                async move |_: ToolCall<()>| {
                    barrier.wait().await;
                    Ok::<_, Infallible>(name)
                },
            ));
        }
    })
    .await;
    let model = ScriptedModel::new()
        .calls([("c1", "second", json!({})), ("c2", "first", json!({}))])
        .reply("Done.");
    // `scripted` times out if the calls ran one after another.
    scripted(&agent, &model, "Go").await.unwrap();
    assert_eq!(last_results(&model), ["result:second", "result:first"]);
}

#[tokio::test]
async fn reaching_the_turn_cap_fails_the_run() {
    let agent = agent(|cx| cx.use_tool(clock())).await.max_turns(2);
    let model = ScriptedModel::new()
        .calls([("c1", "current_time", json!({}))])
        .calls([("c2", "current_time", json!({}))])
        .reply("Never reached.");
    let failed = scripted(&agent, &model, "Go").await;
    assert_eq!(reason(failed), "turn limit (2) reached");
}

#[tokio::test]
async fn use_tools_declares_each_tool_in_order_by_reference() {
    let (clock, lookup) = (clock(), lookup_order());
    let agent = agent(move |cx| cx.use_tools([&lookup, &clock])).await;
    let model = ScriptedModel::new().reply("Done.");
    scripted(&agent, &model, "Go").await.unwrap();
    assert_eq!(
        tool_names(&model.requests()[0]),
        ["lookup_order", "current_time"]
    );
}

#[tokio::test]
async fn a_tool_may_take_a_name_keelrs_has_no_builtin_for() {
    for name in ["task", "finish", "give_up"] {
        let tool = Tool::host(name, "Mine.", async |_: ToolCall<()>| {
            Ok::<_, Infallible>("ran")
        });
        let agent = agent(move |cx| cx.use_tool(tool.clone())).await;
        let reply = scripted(&agent, &ScriptedModel::new().reply("Done."), "Go").await;
        assert!(reply.is_ok(), "{name}: {reply:?}");
    }
}
