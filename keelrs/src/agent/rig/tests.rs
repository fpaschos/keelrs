//! The tool loop, driven on rig-core's scripted model: no network.

use std::convert::Infallible;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rig_core::completion::AssistantContent;
use rig_core::message::{ToolFunction, ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Barrier;

use super::*;
use crate::{AgentContext, Tool, ToolCall};

/// Runs `agent` on one message against a model that replies with `turns`.
async fn scripted(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> Result<Settled, String> {
    let model = MockCompletionModel::from_turns(turns);
    run_with(agent.clone(), "Go".to_owned(), Vec::new(), |_| {
        Ok(model.clone().erase())
    })
    .await
}

/// Every tool result the run accepted, in order, as (text or JSON, is_error).
fn results(settled: &Settled) -> Vec<(String, bool)> {
    let mut results = Vec::new();
    for message in &settled.messages {
        let RigMessage::User { content, .. } = message.0.as_ref() else {
            continue;
        };
        for content in content.iter() {
            if let UserContent::ToolResult(result) = content {
                let text = result
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ToolResultContent::Text(t) => Some(t.text.clone()),
                        ToolResultContent::Json { value } => Some(value.to_string()),
                        ToolResultContent::Image(_) => None,
                    })
                    .collect::<String>();
                results.push((text, result.is_error));
            }
        }
    }
    results
}

fn agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    Agent::new("scripted", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        declare(cx);
        String::new()
    })
}

fn clock() -> Tool {
    Tool::new(
        "current_time",
        "The current time.",
        async |_: ToolCall<()>| Ok::<_, Infallible>("noon"),
    )
}

#[tokio::test]
async fn a_tool_result_reaches_the_model_and_the_run_settles() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let settled = scripted(
        &agent,
        [
            MockTurn::tool_call("c1", "current_time", json!({})),
            MockTurn::text("It is noon."),
        ],
    )
    .await
    .unwrap();
    assert_eq!(settled.reply, "It is noon.");
    assert_eq!(results(&settled), [("noon".to_owned(), false)]);
}

/// One tool call, then the reply "Done.", and the results the run accepted.
async fn one_call(agent: &Agent, name: &str, arguments: Value) -> Vec<(String, bool)> {
    let settled = scripted(
        agent,
        [
            MockTurn::tool_call("c1", name, arguments),
            MockTurn::text("Done."),
        ],
    )
    .await
    .unwrap();
    results(&settled)
}

#[derive(Deserialize, JsonSchema)]
struct Order {
    order_id: String,
}

#[tokio::test]
async fn a_value_reaches_the_model_as_json_and_a_call_gets_its_id() {
    let agent = agent(|cx| {
        cx.use_tool(Tool::new(
            "lookup_order",
            "Look up an order.",
            async |call: ToolCall<Order>| {
                Ok::<_, Infallible>(json!({"order": call.data.order_id, "call": call.id}))
            },
        ));
    });
    let results = one_call(&agent, "lookup_order", json!({"order_id": "A1"})).await;
    assert_eq!(
        results,
        [(r#"{"order":"A1","call":"c1"}"#.to_owned(), false)]
    );
}

#[tokio::test]
async fn an_err_reaches_the_model_as_its_message() {
    let agent = agent(|cx| {
        cx.use_tool(Tool::new(
            "lookup_order",
            "Look up an order.",
            async |call: ToolCall<Order>| {
                Err::<(), _>(io::Error::other(format!("no order {}", call.data.order_id)))
            },
        ));
    });
    let results = one_call(&agent, "lookup_order", json!({"order_id": "Z9"})).await;
    assert_eq!(results, [("no order Z9".to_owned(), true)]);
}

#[tokio::test]
async fn arguments_that_break_the_schema_are_refused_and_the_tool_does_not_run() {
    let ran = Arc::new(AtomicBool::new(false));
    let agent = agent({
        let ran = ran.clone();
        move |cx| {
            let ran = ran.clone();
            cx.use_tool(Tool::new(
                "lookup_order",
                "Look up an order.",
                async move |_: ToolCall<Order>| {
                    ran.store(true, Ordering::SeqCst);
                    Ok::<_, Infallible>("found")
                },
            ));
        }
    });
    let results = one_call(&agent, "lookup_order", json!({"order_id": 5})).await;
    assert!(!ran.load(Ordering::SeqCst));
    let [(text, true)] = results.as_slice() else {
        panic!("expected one error result, got {results:?}");
    };
    assert!(text.contains("do not fit its schema"), "{text}");
}

#[tokio::test]
async fn a_call_past_its_timeout_reaches_the_model_as_timed_out() {
    let agent = agent(|cx| {
        cx.use_tool(
            Tool::new("slow", "Never finishes.", async |_: ToolCall<()>| {
                std::future::pending::<Result<(), Infallible>>().await
            })
            .timeout(Duration::from_millis(10)),
        );
    });
    let results = one_call(&agent, "slow", json!({})).await;
    assert_eq!(
        results,
        [("tool \"slow\" timed out after 10ms".to_owned(), true)]
    );
}

#[tokio::test]
async fn a_call_to_an_undeclared_tool_names_the_available_ones() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let results = one_call(&agent, "issue_refund", json!({})).await;
    assert_eq!(
        results,
        [(
            "tool \"issue_refund\" is not available; available tools: current_time".to_owned(),
            true
        )]
    );
}

#[tokio::test]
async fn one_replys_calls_run_concurrently_and_are_committed_in_call_order() {
    // Each call waits for the other: run one after another, they never finish.
    let barrier = Arc::new(Barrier::new(2));
    let agent = agent(move |cx| {
        for name in ["first", "second"] {
            let barrier = barrier.clone();
            cx.use_tool(Tool::new(
                name,
                "Waits for the other.",
                async move |_: ToolCall<()>| {
                    barrier.wait().await;
                    Ok::<_, Infallible>(name)
                },
            ));
        }
    });
    let both = MockTurn::from_contents([("c1", "second"), ("c2", "first")].map(|(id, name)| {
        let function = ToolFunction::new(ToolName::new(name).unwrap(), json!({}));
        AssistantContent::ToolCall(rig_core::message::ToolCall::from_wire(id, function))
    }));
    let settled = tokio::time::timeout(
        Duration::from_secs(5),
        scripted(&agent, [both, MockTurn::text("Done.")]),
    )
    .await
    .expect("the calls ran one after another")
    .unwrap();
    assert_eq!(
        results(&settled),
        [("second".to_owned(), false), ("first".to_owned(), false)]
    );
}

#[tokio::test]
async fn reaching_the_turn_cap_fails_the_run() {
    let agent = agent(|cx| cx.use_tool(clock())).max_turns(2);
    let reason = scripted(
        &agent,
        [
            MockTurn::tool_call("c1", "current_time", json!({})),
            MockTurn::tool_call("c2", "current_time", json!({})),
            MockTurn::text("Never reached."),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(reason, "turn limit (2) reached");
}
