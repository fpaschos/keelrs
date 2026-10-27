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
use crate::skills::catalog::catalog;
use crate::skills::tools::builtin;
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
                let skills = cx.skills()?;
                // The built-in skill tools follow the declared ones and are pinned with them.
                let tools: Vec<Tool> = cx.tools()?.iter().cloned().chain(builtin(skills)).collect();
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
                // The skill catalog follows the instructions (How a skill reaches the model).
                let parts: Vec<String> = instructions.into_iter().chain(catalog(skills)).collect();
                spec.preamble = (!parts.is_empty()).then(|| parts.join("\n\n"));
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
    // The run only emits calls the pinned render declared.
    let tool = tools
        .get(&**call.name())
        .ok_or_else(|| format!("tool \"{}\" was not declared this turn", call.name()))?;
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
mod tests;
