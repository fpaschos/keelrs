//! The Rig integration: drives one Rig `AgentRun` per dispatch; the only module where Rig types appear (ADR 0004).
//!
//! Each render's tools go to the model and stay pinned for the `CallTools`
//! step that follows its model call (How a tool call runs).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::future::join_all;
use parking_lot::Mutex;
use rig_agent::run::{
    AgentRun, AgentRunStep, InvalidToolCallAction, ModelTurn, ModelTurnOutcome, PendingToolCall,
    PromptError, RunSpec, ToolAnswer, prepare_request,
};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::{
    CompletionRequest, Effort, GenerationOptions, Reasoning, ToolDefinition,
};
use rig_core::message::{Message as RigMessage, ToolCall as RigToolCall, ToolFunction, ToolName};
use rig_core::operation::Completion;
use rig_core::tool::feedback::arguments_refusal;
use rig_core::tool::{ToolErrorKind, ToolExecutionError, ToolOutput, ToolResult};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::agent::InstanceAgent;
use crate::builtin::{self, Caches};
use crate::model::Thinking;
use crate::sandbox::Sandbox;
use crate::skills::catalog::catalog;
use crate::skills::tools::skill_tools;
use crate::tool::{Called, FailKind, Failed};
use crate::{Model, Tool};

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
    /// The structured result `finish_structured` recorded, when one was asked for.
    pub(crate) structured: Option<Value>,
}

/// The reserved tool the model hands a structured result over with.
pub(crate) const FINISH_STRUCTURED: &str = "finish_structured";

/// The structured result one dispatch asks for: `T`'s schema, its doc comment,
/// and a check that a value deserializes into `T`.
#[derive(Debug, Clone)]
pub(crate) struct Structured {
    /// `finish_structured`'s parameters: `T`'s schema, wrapped as `{result}` when not an object.
    schema: Value,
    /// Whether the value travels as `{"result": value}`.
    wrapped: bool,
    /// `T`'s root doc comment.
    doc: Option<String>,
    /// Deserializes a value into `T`; the error is what the model reads.
    check: fn(Value) -> Result<(), String>,
}

impl Structured {
    /// The structured result `T`.
    pub(crate) fn of<T: DeserializeOwned + JsonSchema>() -> Self {
        // `args_schema` drops the root doc comment and inlines every subschema.
        let doc = schemars::schema_for!(T)
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut schema = args_schema::<T>();
        let wrapped = schema.get("type").and_then(Value::as_str) != Some("object");
        if wrapped {
            schema = serde_json::json!({
                "type": "object",
                "properties": {"result": schema},
                "required": ["result"],
                "additionalProperties": false,
            });
        }
        Self {
            schema,
            wrapped,
            doc,
            check: |value| {
                serde_json::from_value::<T>(value)
                    .map(drop)
                    .map_err(|e| e.to_string())
            },
        }
    }

    /// The `finish_structured` tool, recording the first value that fits in `recorded`.
    fn tool(&self, recorded: Arc<Mutex<Option<Value>>>) -> Tool {
        let description = match &self.doc {
            Some(doc) => format!("{FINISH_DESCRIPTION}\n\n{doc}"),
            None => FINISH_DESCRIPTION.to_owned(),
        };
        let (wrapped, check) = (self.wrapped, self.check);
        let run = move |arguments: Value, _: String, _: Sandbox| {
            let recorded = recorded.clone();
            Box::pin(async move {
                let value = match arguments {
                    Value::Object(mut wrapper) if wrapped => {
                        wrapper.remove("result").unwrap_or_default()
                    }
                    value => value,
                };
                check(value.clone()).map_err(|e| {
                    let message = format!("`{FINISH_STRUCTURED}`'s arguments do not fit: {e}");
                    Failed::new(FailKind::InvalidArgs, message)
                })?;
                let mut recorded = recorded.lock();
                if recorded.is_some() {
                    return Ok(Value::from("already finished"));
                }
                *recorded = Some(value);
                Ok(Value::from("Recorded."))
            }) as Pin<Box<dyn Future<Output = Called> + Send>>
        };
        Tool {
            name: FINISH_STRUCTURED.into(),
            description: description.into(),
            schema: Arc::new(self.schema.clone()),
            timeout: None,
            run: Arc::new(run),
        }
    }
}

/// How many times a dispatch that asked for a structured result is asked again
/// after ending with text.
const MAX_STRUCTURED_REASKS: usize = 3;

/// What follows a message that asks for a structured result.
const FOOTER: &str = "When the task is complete, call `finish_structured` with your answer as \
    its arguments. The arguments are checked against its schema; if they don't match you get an \
    error and can try again. Don't answer in plain text: only a successful `finish_structured` \
    call counts.";

/// The message a re-ask sends.
const REASK: &str = "You ended without calling `finish_structured`. Call it now with your answer.";

/// What `finish_structured` tells the model, before `T`'s doc comment.
const FINISH_DESCRIPTION: &str = "Call this once, when the task is complete, with your final \
    answer as the arguments. They are validated against the schema; the first successful call wins.";

/// Runs the instance's `agent` on `text` after `history`, its built-in tools remembering
/// in `caches`; an error is the failure reason, as plain text. With `structured`, the
/// run must end with a successful `finish_structured` call.
pub(crate) async fn run(
    agent: InstanceAgent,
    text: String,
    history: Vec<Message>,
    structured: Option<Structured>,
    caches: &mut Caches,
) -> Result<Settled, String> {
    run_with(agent, text, history, structured, caches, |model| {
        Catalog::builtin()
            .connect(model.id.as_str())
            .map_err(|e| e.to_string())
    })
    .await
}

/// [`run`], connecting the declared model with `connect`.
async fn run_with(
    agent: InstanceAgent,
    text: String,
    history: Vec<Message>,
    structured: Option<Structured>,
    caches: &mut Caches,
    connect: impl Fn(&Model) -> Result<DynModel<Completion>, String>,
) -> Result<Settled, String> {
    let max_turns = agent.max_turns;
    let history: Vec<Arc<RigMessage>> = history.into_iter().map(|m| m.0).collect();
    // The footer is stored with the message it asks a structured result for.
    let text = match structured {
        Some(_) => format!("{text}\n\n{FOOTER}"),
        None => text,
    };
    let mut run = AgentRun::new(RigMessage::user(text))
        .with_history(history.iter().cloned())
        .max_turns(max_turns);
    // A re-ask rebuilds the run: what the earlier runs accepted, the model
    // calls they made, and how many re-asks there were.
    let mut earlier: Vec<Arc<RigMessage>> = Vec::new();
    let (mut turns_used, mut reasks) = (0, 0);
    // Connected from the first render and kept for every turn (use_model behaviour).
    let mut connected = None;
    // The last render's tools, by name, and its sandbox, for the `CallTools`
    // step after its model call.
    let mut pinned: Option<(HashMap<String, Tool>, Sandbox)> = None;
    // What `finish_structured` recorded this dispatch, and the tool itself.
    let recorded = Arc::new(Mutex::new(None));
    let finish = structured.map(|s| s.tool(recorded.clone()));
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
                let (cx, returned) = agent.render();
                let declared = cx.model()?;
                let sb = cx.sandbox()?.clone();
                let instructions = cx.instructions(&returned)?;
                let skills = cx.skills()?;
                let names = cx.builtin_names()?;
                let declared_tools = cx.tools()?;
                // Flue's order: the sandbox built-ins, the skill tools, then the declared ones.
                // The cache is taken only once the render is valid, so a failed one keeps it.
                let tools: Vec<Tool> = builtin::tools(&sb, &caches.seen(sb.id()), &names)
                    .into_iter()
                    .chain(skill_tools(skills))
                    .chain(declared_tools.iter().cloned())
                    .chain(finish.clone())
                    .collect();
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
                pinned = Some((
                    tools
                        .iter()
                        .map(|tool| (tool.name.to_string(), tool.clone()))
                        .collect(),
                    sb,
                ));

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
                let (tools, sandbox) = pinned
                    .as_ref()
                    .ok_or("the run called tools before any render")?;
                // One reply's calls run concurrently; Rig commits them in call order.
                let answers = join_all(calls.into_iter().map(|call| answer(call, tools, sandbox)))
                    .await
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                run.answer_all(answers).map_err(|e| e.to_string())?;
                // A recorded result ends the dispatch once its whole batch has run.
                if let Some(value) = recorded.lock().take() {
                    return Ok(Settled {
                        reply: String::new(),
                        messages: earlier
                            .into_iter()
                            .chain(run.messages().iter().cloned())
                            .map(Message)
                            .collect(),
                        structured: Some(value),
                    });
                }
            }
            // The model ended with text, not the structured result: ask again.
            AgentRunStep::Done(response) if finish.is_some() => {
                if reasks == MAX_STRUCTURED_REASKS {
                    return Err(format!(
                        "ended without calling `{FINISH_STRUCTURED}` after {reasks} re-asks"
                    ));
                }
                reasks += 1;
                turns_used += run.turn();
                earlier.extend(response.messages.into_iter().map(Arc::new));
                run = AgentRun::new(RigMessage::user(REASK))
                    .with_history(history.iter().chain(&earlier).cloned())
                    .max_turns(max_turns.saturating_sub(turns_used));
            }
            AgentRunStep::Done(response) => {
                return Ok(Settled {
                    reply: response.output(),
                    messages: response
                        .messages
                        .into_iter()
                        .map(|m| Message(Arc::new(m)))
                        .collect(),
                    structured: None,
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
    sandbox: &Sandbox,
) -> Result<ToolAnswer, String> {
    let exec = match call {
        PendingToolCall::Execute(call) => call,
        // Arguments that are not a JSON object: Rig's feedback asks for a retry.
        PendingToolCall::Malformed(call) => return Ok(call.answer(None)),
        other => return Err(format!("unsupported tool call {other:?}")),
    };
    // The run only emits calls the pinned render declared.
    let tool = tools
        .get(&**exec.name())
        .ok_or_else(|| format!("tool \"{}\" was not declared this turn", exec.name()))?;
    let id = exec.id().wire().into_owned();
    let called = self::call(tool, exec.arguments(), id, sandbox).await;
    Ok(exec.answer(result(called)))
}

/// What Rig is answered with for `called`: a string is text, `()` empty
/// text, any other value JSON, and a failure keeps its kind.
fn result(called: Called) -> ToolResult {
    match called {
        Ok(Value::String(text)) => ToolResult::success(ToolOutput::text(text)),
        Ok(Value::Null) => ToolResult::success(ToolOutput::text("")),
        Ok(value) => ToolResult::success(ToolOutput::json(value)),
        Err(failed) => ToolResult::failed(ToolExecutionError::new(
            kind_of(failed.kind),
            failed.message,
        )),
    }
}

/// Runs one call of `tool` exactly as a turn does: the schema refusal,
/// decoding, the run, the timeout. What the model reads is `Ok` or `Err`.
pub(crate) async fn call(tool: &Tool, arguments: Value, id: String, sandbox: &Sandbox) -> Called {
    // Rebuilt from the name and arguments: Rig answers a call with unparsed
    // arguments as malformed, so one that reaches a tool never has them.
    let name =
        ToolName::new(&*tool.name).map_err(|e| Failed::new(FailKind::Other, e.to_string()))?;
    let rig_call = RigToolCall::from_wire(id.clone(), ToolFunction::new(name, arguments));
    if let Some(refusal) = arguments_refusal(&tool.schema, &rig_call) {
        return Err(Failed::new(FailKind::InvalidArgs, refusal));
    }
    // The object the refusal checked, as a turn's `ExecCall::arguments` gives it.
    let arguments = Value::Object(rig_call.function.arguments);
    let running = (tool.run)(arguments, id, sandbox.clone());
    match tool.timeout {
        Some(limit) => tokio::time::timeout(limit, running)
            .await
            .unwrap_or_else(|_| {
                let message = format!("tool \"{}\" timed out after {limit:?}", tool.name);
                Err(Failed::new(FailKind::Timeout, message))
            }),
        None => running.await,
    }
}

/// Rig's kind for a keelrs one.
fn kind_of(kind: FailKind) -> ToolErrorKind {
    match kind {
        FailKind::InvalidArgs => ToolErrorKind::InvalidArgs,
        FailKind::NotFound => ToolErrorKind::NotFound,
        FailKind::PermissionDenied => ToolErrorKind::PermissionDenied,
        FailKind::Timeout => ToolErrorKind::Timeout,
        FailKind::Other => ToolErrorKind::Other,
    }
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
