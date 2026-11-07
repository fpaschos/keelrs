//! The Rig integration: steps one Rig `AgentRun` per dispatch; the only module where Rig types appear (ADR 0004).
//!
//! Each render's tools go to the model and stay pinned for the `CallTools`
//! step that follows its model call (How a tool call runs).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use rig_agent::run::{
    AgentRun, AgentRunStep, InvalidToolCallAction, ModelTurn, ModelTurnOutcome, PendingToolCall,
    PromptError, RetryRequest, RunSpec, ToolAnswer, prepare_request,
};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::message::turn_failure;
use rig_core::completion::{
    CompletionRequest, Effort, GenerationOptions, Reasoning, ToolDefinition,
};
use rig_core::conversation::pairing;
use rig_core::message::{
    AssistantContent, CallId, Message as RigMessage, ToolCall as RigToolCall, ToolFunction,
    ToolName, ToolResult as RigToolResult, UserContent,
};
use rig_core::operation::Completion;
use rig_core::tool::feedback::arguments_refusal;
use rig_core::tool::{ToolErrorKind, ToolExecutionError, ToolOutput, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{InstanceAgent, SettledValue};
use crate::builtin::structured::{
    FINISH_STRUCTURED, FOOTER, MAX_STRUCTURED_REASKS, REASK, Structured,
};
use crate::builtin::{self, Caches};
use crate::event::Event;
use crate::model::Thinking;
use crate::sandbox::Sandbox;
use crate::skills::catalog::catalog;
use crate::skills::tools::skill_tools;
use crate::tool::{Called, FailKind, Failed};
use crate::{Model, Tool};

/// One message a run accepted; opaque so Rig's type stays in this module.
#[derive(Debug, Clone)]
pub(crate) struct Message(Arc<RigMessage>);

impl Serialize for Message {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RigMessage::deserialize(deserializer).map(|m| Self(Arc::new(m)))
    }
}

/// One tool call's result, as the next user message carries it; opaque so
/// Rig's type stays in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ToolResultPart(RigToolResult);

/// Adds `part` to `history`: into the tool results that follow the last
/// assistant message, kept in its call order, or as a new user message.
pub(crate) fn push_result(history: &mut Vec<Message>, part: &ToolResultPart) {
    let order: Vec<CallId> = history
        .iter()
        .rev()
        .find_map(|m| match m.0.as_ref() {
            RigMessage::Assistant(reply) => Some(
                reply
                    .content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(call) => Some(call.id.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let rank = |id: &CallId| {
        order
            .iter()
            .position(|call| call == id)
            .unwrap_or(usize::MAX)
    };
    // Only a non-empty message of nothing but tool results takes more of them.
    let results_last = matches!(
        history.last().map(|m| m.0.as_ref()),
        Some(RigMessage::User { content })
            if !content.is_empty()
                && content.iter().all(|c| matches!(c, UserContent::ToolResult(_)))
    );
    if !results_last {
        history.push(Message(Arc::new(RigMessage::User {
            content: Vec::new(),
        })));
    }
    let last = history.last_mut().expect("pushed above");
    // Copies the message only while another history shares it.
    if let RigMessage::User { content } = Arc::make_mut(&mut last.0) {
        let mine = rank(&part.0.call);
        let at = content
            .iter()
            .position(|c| matches!(c, UserContent::ToolResult(r) if rank(&r.call) > mine))
            .unwrap_or(content.len());
        content.insert(at, UserContent::ToolResult(part.0.clone()));
    }
}

/// A result saying `why` for each tool call at the end of `history` that has none:
/// providers reject a call left unanswered.
pub(crate) fn close_pending(history: &[Message], why: &str) -> Vec<ToolResultPart> {
    let messages: Vec<&RigMessage> = history.iter().map(|m| m.0.as_ref()).collect();
    let calls = pairing::pending_calls(&messages);
    let RigMessage::User { content } = pairing::close_pending_with(&calls, why) else {
        return Vec::new();
    };
    content
        .into_iter()
        .filter_map(|c| match c {
            UserContent::ToolResult(r) => Some(ToolResultPart(r)),
            _ => None,
        })
        .collect()
}

/// What one [`Run::step`] produced.
#[derive(Debug)]
pub(crate) enum Step {
    /// Events to append before the next step.
    Append(Vec<Event>),
    /// The run's last events, appended with its settled value.
    Settled(Vec<Event>, SettledValue),
}

/// Connects the declared model.
pub(crate) type Connect = Box<dyn Fn(&Model) -> Result<DynModel<Completion>, String> + Send + Sync>;

/// A tool call running; it yields its id and the answer.
type Running = BoxFuture<'static, Result<(CallId, ToolAnswer), String>>;

/// One dispatch's run on Rig's `AgentRun`, driven one [`step`](Self::step) at a
/// time so the caller stores what each step produced before the next one.
pub(crate) struct Run {
    agent: InstanceAgent,
    run: AgentRun,
    structured: Option<Structured>,
    reasks: usize,
    /// What this instance's built-in tools remember; handed back at the end.
    caches: Caches,
    connect: Connect,
    /// Connected from the first render and kept for every turn (use_model behaviour).
    connected: Option<(DynModel<Completion>, Option<Thinking>)>,
    /// The last render's tools, by name, and its sandbox, for the `CallTools`
    /// step after its model call.
    pinned: Option<(HashMap<String, Tool>, Sandbox)>,
    /// How many of the run's messages earlier steps produced.
    produced: usize,
    /// The step taken after a model call, to run next.
    next: Option<AgentRunStep>,
    /// The tool calls of the pending batch, started at the next step.
    calls: FuturesUnordered<Running>,
}

impl fmt::Debug for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Run")
            .field("turn", &self.run.turn())
            .finish_non_exhaustive()
    }
}

impl Run {
    /// A run of the instance's `agent` on `text` after `history`, its built-in
    /// tools remembering in `caches`. With `structured`, the run must end with
    /// a successful `finish_structured` call.
    pub(crate) fn new(
        agent: InstanceAgent,
        text: String,
        history: Vec<Message>,
        structured: Option<Structured>,
        caches: Caches,
    ) -> Self {
        Self::with_connect(
            agent,
            text,
            history,
            structured,
            caches,
            Box::new(|model| {
                Catalog::builtin()
                    .connect(model.id.as_str())
                    .map_err(|e| e.to_string())
            }),
        )
    }

    /// [`Run::new`], connecting the declared model with `connect`.
    pub(crate) fn with_connect(
        agent: InstanceAgent,
        text: String,
        history: Vec<Message>,
        structured: Option<Structured>,
        caches: Caches,
        connect: Connect,
    ) -> Self {
        let history: Vec<Arc<RigMessage>> = history.into_iter().map(|m| m.0).collect();
        // The footer is stored with the message it asks a structured result for.
        let text = match structured {
            Some(_) => format!("{text}\n\n{FOOTER}"),
            None => text,
        };
        let run = AgentRun::new(RigMessage::user(text))
            .with_history(history)
            .max_turns(agent.max_turns);
        Self {
            agent,
            run,
            structured,
            reasks: 0,
            caches,
            connect,
            connected: None,
            pinned: None,
            produced: 0,
            next: None,
            calls: FuturesUnordered::new(),
        }
    }

    /// What the instance's built-in tools remember, once the run is over.
    pub(crate) fn into_caches(self) -> Caches {
        self.caches
    }

    /// Takes the next step: one model call (its answer), one finished tool
    /// result, or the final answer; an error is the failure reason, as plain
    /// text. Owned and `'static`, so the caller can keep it across awaits.
    pub(crate) async fn step(mut self) -> (Self, Result<Step, String>) {
        let step = self.advance().await;
        (self, step)
    }

    async fn advance(&mut self) -> Result<Step, String> {
        let max_turns = self.agent.max_turns;
        loop {
            // C: the batch's next result to finish.
            if let Some(answered) = self.calls.next().await {
                let (id, answer) = answered?;
                self.run.answer(answer).map_err(|e| e.to_string())?;
                let events = vec![Event::ToolResult(self.result_of(&id)?)];
                if !self.calls.is_empty() {
                    return Ok(Step::Append(events));
                }
                // Rig commits the batch's results as one message, produced above one by one.
                self.produced = self.run.messages().len();
                // A recorded result ends the dispatch once its whole batch has run.
                return match self.structured.as_ref().and_then(Structured::recorded) {
                    Some(value) => Ok(Step::Settled(
                        events,
                        SettledValue {
                            text: String::new(),
                            structured: Some(value),
                        },
                    )),
                    None => Ok(Step::Append(events)),
                };
            }
            let step = match self.next.take() {
                Some(step) => step,
                None => match self.run.next_step() {
                    Ok(step) => step,
                    Err(PromptError::MaxTurns { .. }) => {
                        return Err(format!("turn limit ({max_turns}) reached"));
                    }
                    Err(e) => return Err(e.to_string()),
                },
            };
            match step {
                AgentRunStep::CallModel {
                    prompt, history, ..
                } => {
                    self.call_model(&prompt, &history).await?;
                    // B: the model's answer, once the run has taken it.
                    self.next = Some(match self.run.next_step() {
                        Ok(step) => step,
                        Err(PromptError::MaxTurns { .. }) => {
                            return Err(format!("turn limit ({max_turns}) reached"));
                        }
                        Err(e) => return Err(e.to_string()),
                    });
                    if let Some(AgentRunStep::Done(_)) = &self.next {
                        continue;
                    }
                    let events = self.produced();
                    if !events.is_empty() {
                        return Ok(Step::Append(events));
                    }
                }
                AgentRunStep::CallTools { calls } => {
                    let (tools, sandbox) = self
                        .pinned
                        .as_ref()
                        .ok_or("the run called tools before any render")?;
                    // One reply's calls run concurrently; Rig commits them in call order.
                    for call in calls {
                        let (tools, sandbox) = (tools.clone(), sandbox.clone());
                        self.calls.push(Box::pin(
                            async move { answer(call, &tools, &sandbox).await },
                        ));
                    }
                    // B, when the model call's step did not produce it.
                    let events = self.produced();
                    if !events.is_empty() {
                        return Ok(Step::Append(events));
                    }
                }
                AgentRunStep::Done(response) => {
                    // D: the final answer and settled together.
                    return Ok(Step::Settled(
                        self.produced(),
                        SettledValue {
                            text: response.output(),
                            structured: None,
                        },
                    ));
                }
            }
        }
    }

    /// The run's messages no step has produced yet, as events.
    fn produced(&mut self) -> Vec<Event> {
        let messages = &self.run.messages()[self.produced..];
        self.produced += messages.len();
        messages
            .iter()
            .map(|m| Event::Message(Message(m.clone())))
            .collect()
    }

    /// The result the run built for the call `id`.
    fn result_of(&self, id: &CallId) -> Result<ToolResultPart, String> {
        let find = |message: &RigMessage| match message {
            RigMessage::User { content } => content.iter().find_map(|c| match c {
                UserContent::ToolResult(r) if r.call == *id => Some(ToolResultPart(r.clone())),
                _ => None,
            }),
            _ => None,
        };
        // A finished batch is the run's last message; an open one shows only
        // at the end of its full history, which Rig builds by copying.
        let found = if self.calls.is_empty() {
            self.run.messages().last().and_then(|m| find(m))
        } else {
            self.run.full_history().last().and_then(find)
        };
        found.ok_or_else(|| format!("no result for tool call `{id}`"))
    }

    /// Renders the agent and calls the model once, leaving its answer with the run.
    async fn call_model(
        &mut self,
        prompt: &RigMessage,
        history: &[Arc<RigMessage>],
    ) -> Result<(), String> {
        let max_turns = self.agent.max_turns;
        // The agent function runs again before every turn (ADR 0004).
        let (cx, returned) = self.agent.render();
        let declared = cx.model()?;
        let sb = cx.sandbox()?.clone();
        let instructions = cx.instructions(&returned)?;
        let skills = cx.skills()?;
        let names = cx.builtin_names()?;
        let declared_tools = cx.tools()?;
        // Flue's order: the sandbox built-ins, the skill tools, then the declared ones.
        // The cache is taken only once the render is valid, so a failed one keeps it.
        let tools: Vec<Tool> = builtin::tools(&sb, &self.caches.seen(sb.id()), &names)
            .into_iter()
            .chain(skill_tools(skills))
            .chain(declared_tools.iter().cloned())
            .chain(self.structured.as_ref().map(|s| s.tool.clone()))
            .collect();
        let (model, thinking) = match &mut self.connected {
            Some(connected) => connected,
            None => self
                .connected
                .insert(((self.connect)(declared)?, declared.thinking)),
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
        self.pinned = Some((
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
            history,
            definitions,
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        let policy = prepared.policy.clone();
        let response = model
            .call(prepared.apply(CompletionRequest::new(prompt.clone())))
            .await
            .map_err(|e| e.to_string())?;
        let run = &mut self.run;
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
        // A text ending, when a structured result is asked for, is re-asked
        // in the same run: the reply stays, the re-ask follows it.
        if self.structured.is_some()
            && turn_failure(
                &response.choice,
                response.head().stop.as_ref(),
                response.finish_reason().as_ref(),
            )
            .is_none()
            && !response
                .choice
                .iter()
                .any(|item| matches!(item, AssistantContent::ToolCall(_)))
        {
            if self.reasks == MAX_STRUCTURED_REASKS {
                return Err(format!(
                    "ended without calling `{FINISH_STRUCTURED}` after {} re-asks",
                    self.reasks
                ));
            }
            if run.turn() >= max_turns {
                return Err(format!(
                    "ended without calling `{FINISH_STRUCTURED}`: turn limit ({max_turns}) reached"
                ));
            }
            self.reasks += 1;
            run.retry_model_turn(RetryRequest::Feedback(REASK.to_owned()))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Runs one call with its pinned tool and answers it, with the call's id; a model mistake is an
/// error result the model reads, never a failed run.
async fn answer(
    call: PendingToolCall,
    tools: &HashMap<String, Tool>,
    sandbox: &Sandbox,
) -> Result<(CallId, ToolAnswer), String> {
    let exec = match call {
        PendingToolCall::Execute(call) => call,
        // Arguments that are not a JSON object: Rig's feedback asks for a retry.
        PendingToolCall::Malformed(call) => return Ok((call.id().clone(), call.answer(None))),
        other => return Err(format!("unsupported tool call {other:?}")),
    };
    // The run only emits calls the pinned render declared.
    let tool = tools
        .get(&**exec.name())
        .ok_or_else(|| format!("tool \"{}\" was not declared this turn", exec.name()))?;
    let id = exec.id().wire().into_owned();
    let called = self::call(tool, exec.arguments(), id, sandbox).await;
    Ok((exec.id().clone(), exec.answer(result(called))))
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
