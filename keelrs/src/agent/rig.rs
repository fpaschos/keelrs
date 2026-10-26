//! The Rig integration: drives one Rig `AgentRun` per dispatch; the only module where Rig types appear (ADR 0004).
//!
//! Each render's tools go to the model and stay pinned for the `CallTools`
//! step that follows its model call (How a tool call runs).

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::join_all;
use rig_agent::run::{
    AgentRun, AgentRunStep, InvalidToolCallAction, ModelTurn, ModelTurnOutcome, PendingToolCall,
    PromptError, RunSpec, ToolAnswer, prepare_request,
};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::{
    CompletionRequest, Effort, GenerationOptions, Reasoning, ToolDefinition,
};
use rig_core::message::{Message as RigMessage, ToolName};
use rig_core::operation::Completion;
use rig_core::tool::feedback::arguments_refusal;
use rig_core::tool::{ToolExecutionError, ToolOutput, ToolResult};
use serde_json::Value;

use crate::model::Thinking;
use crate::{Agent, Model, Tool};

/// One message a run accepted; opaque so Rig's type stays in this module.
#[derive(Debug, Clone)]
pub(crate) struct Message(Arc<RigMessage>);

/// A run that settled with a reply.
#[derive(Debug)]
pub(crate) struct Settled {
    /// The final assistant text.
    pub(crate) reply: String,
    /// What the run accepted, prompt first, excluding the input history.
    pub(crate) messages: Vec<Message>,
}

/// Runs `agent` on `text` after `history`; an error is the failure reason, as plain text.
pub(crate) async fn run(
    agent: Agent,
    text: String,
    history: Vec<Message>,
) -> Result<Settled, String> {
    run_with(agent, text, history, |model| {
        Catalog::builtin()
            .connect(model.id.as_str())
            .map_err(|e| e.to_string())
    })
    .await
}

/// [`run`], connecting the declared model with `connect`.
async fn run_with(
    agent: Agent,
    text: String,
    history: Vec<Message>,
    connect: impl Fn(&Model) -> Result<DynModel<Completion>, String>,
) -> Result<Settled, String> {
    let max_turns = agent.max_turns;
    let mut run = AgentRun::new(RigMessage::user(text))
        .with_history(history.into_iter().map(|m| m.0))
        .max_turns(max_turns);
    // Connected from the first render and kept for every turn (use_model behaviour).
    let mut connected = None;
    // The last render's tools, by name, for the `CallTools` step after its model call.
    let mut pinned = HashMap::new();
    loop {
        let step = match run.next_step() {
            Ok(step) => step,
            Err(PromptError::MaxTurns { .. }) => {
                return Err(format!("turn limit ({max_turns}) reached"));
            }
            Err(e) => return Err(e.to_string()),
        };
        match step {
            AgentRunStep::CallModel {
                prompt, history, ..
            } => {
                // The agent function runs again before every turn (ADR 0004).
                let (cx, returned) = agent.declare();
                let declared = cx.model()?;
                let instructions = cx.instructions(&returned)?;
                let tools = cx.tools()?;
                let (model, thinking) = match &mut connected {
                    Some(connected) => connected,
                    None => connected.insert((connect(declared)?, declared.thinking)),
                };
                let definitions = tools
                    .iter()
                    .map(|tool| {
                        let name = ToolName::new(&*tool.name).map_err(|e| e.to_string())?;
                        let schema = (*tool.schema).clone();
                        Ok(Arc::new(ToolDefinition::new(
                            name,
                            &*tool.description,
                            schema,
                        )))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                pinned = tools
                    .iter()
                    .map(|tool| (tool.name.to_string(), tool.clone()))
                    .collect();

                let mut spec = RunSpec::new();
                spec.preamble = instructions;
                if let Some(thinking) = thinking {
                    spec.options = GenerationOptions::default().reasoning(reasoning(*thinking));
                }
                let prepared = prepare_request(
                    &spec,
                    &model.capabilities().completion,
                    &history,
                    definitions,
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;
                let policy = prepared.policy.clone();
                let response = model
                    .call(prepared.apply(CompletionRequest::new((*prompt).clone())))
                    .await
                    .map_err(|e| e.to_string())?;
                let mut outcome = run
                    .model_response(ModelTurn::from_policy(&response, policy))
                    .map_err(|e| e.to_string())?;
                // A call to a tool this render did not declare is skipped with
                // the names it did; its siblings are re-issued next turn.
                while let ModelTurnOutcome::NeedsResolution(invalid) = outcome {
                    let available = match invalid.available_tools.as_slice() {
                        [] => "none".to_owned(),
                        names => names.join(", "),
                    };
                    let skip = InvalidToolCallAction::skip(format!(
                        "tool \"{}\" is not available; available tools: {available}",
                        invalid.tool_name
                    ));
                    outcome = run
                        .resolve_invalid_tool_call(skip)
                        .map_err(|e| e.to_string())?;
                }
            }
            AgentRunStep::CallTools { calls } => {
                // One reply's calls run concurrently; Rig commits them in call order.
                let answers = join_all(calls.into_iter().map(|call| answer(call, &pinned)))
                    .await
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                run.answer_all(answers).map_err(|e| e.to_string())?;
            }
            AgentRunStep::Done(response) => {
                return Ok(Settled {
                    reply: response.output(),
                    messages: response
                        .messages
                        .into_iter()
                        .map(|m| Message(Arc::new(m)))
                        .collect(),
                });
            }
        }
    }
}

/// Runs one call with its pinned tool and answers it; a model mistake is an
/// error result the model reads, never a failed run.
async fn answer(
    call: PendingToolCall,
    tools: &HashMap<String, Tool>,
) -> Result<ToolAnswer, String> {
    let call = match call {
        PendingToolCall::Execute(call) => call,
        // Arguments that are not a JSON object: Rig's feedback asks for a retry.
        PendingToolCall::Malformed(call) => return Ok(call.answer(None)),
        other => return Err(format!("unsupported tool call {other:?}")),
    };
    let Some(tool) = tools.get(&**call.name()) else {
        let missing =
            ToolExecutionError::not_found(format!("tool \"{}\" is not available", call.name()));
        return Ok(call.answer(ToolResult::failed(missing)));
    };
    if let Some(refusal) = arguments_refusal(&tool.schema, call.tool_call()) {
        let refused = ToolExecutionError::invalid_args(refusal);
        return Ok(call.answer(ToolResult::failed(refused)));
    }
    let running = (tool.run)(call.arguments(), call.id().wire().into_owned());
    let called = match tool.timeout {
        Some(limit) => tokio::time::timeout(limit, running)
            .await
            .unwrap_or_else(|_| Err(format!("tool \"{}\" timed out after {limit:?}", tool.name))),
        None => running.await,
    };
    // A string is text, `()` empty text, any other value JSON.
    Ok(call.answer(match called {
        Ok(Value::String(text)) => ToolResult::success(ToolOutput::text(text)),
        Ok(Value::Null) => ToolResult::success(ToolOutput::text("")),
        Ok(value) => ToolResult::success(ToolOutput::json(value)),
        Err(message) => ToolResult::failed(ToolExecutionError::other(message)),
    }))
}

/// The JSON schema the model is sent for a tool's `Args`; `()` takes an
/// empty object.
pub(crate) fn args_schema<Args: schemars::JsonSchema>() -> serde_json::Value {
    let schema = rig_core::tool::args_schema::<Args>();
    if schema.get("type").and_then(|t| t.as_str()) == Some("null") {
        return serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false});
    }
    schema
}

/// Maps keelrs's thinking level 1:1 onto Rig's.
fn reasoning(thinking: Thinking) -> Reasoning {
    match thinking {
        Thinking::Off => Reasoning::Off,
        Thinking::Minimal => Effort::Minimal.into(),
        Thinking::Low => Effort::Low.into(),
        Thinking::Medium => Effort::Medium.into(),
        Thinking::High => Effort::High.into(),
        Thinking::XHigh => Effort::XHigh.into(),
        Thinking::Max => Effort::Max.into(),
    }
}

#[cfg(test)]
mod tests {
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
        let both =
            MockTurn::from_contents([("c1", "second"), ("c2", "first")].map(|(id, name)| {
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
}
