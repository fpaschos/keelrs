//! The stepper: one dispatch's run on Rig's `AgentRun`, one step at a time.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use rig_agent::run::{
    AgentRun, AgentRunStep, InvalidToolCallAction, ModelTurn, ModelTurnOutcome, PromptError,
    RetryRequest, RunSpec, ToolAnswer, prepare_request,
};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::message::turn_failure;
use rig_core::completion::{
    CompletionRequest, Effort, GenerationOptions, Reasoning, ToolDefinition,
};
use rig_core::message::{AssistantContent, CallId, Message as RigMessage, ToolName, UserContent};
use rig_core::operation::Completion;

use super::call::answer;
use super::history::{Message, ToolResultPart};
use crate::agent::{SettledValue, TurnSetup};
use crate::builtin::structured::{
    FINISH_STRUCTURED, FOOTER, MAX_STRUCTURED_REASKS, REASK, Structured,
};
use crate::builtin::{self, Caches};
use crate::event::Event;
use crate::model::Thinking;
use crate::sandbox::Sandbox;
use crate::skills::catalog::catalog;
use crate::skills::tools::skill_tools;
use crate::{Model, Tool};

/// What one [`Run::step`] produced.
#[derive(Debug)]
pub(crate) enum Step {
    /// Events to append before the next step.
    Append(Vec<Event>),
    /// The run's last events, appended with its settled value.
    Settled(Vec<Event>, SettledValue),
}

/// Connects the declared model; shared by every run of a runtime.
pub(crate) type Connect = Arc<dyn Fn(&Model) -> Result<DynModel<Completion>, String> + Send + Sync>;

/// Connects the declared model through Rig's built-in catalog.
pub(crate) fn connect_catalog() -> Connect {
    let catalog = Catalog::builtin();
    Arc::new(move |model| {
        catalog
            .connect(model.id.as_str())
            .map_err(|e| e.to_string())
    })
}

/// A tool call running; it yields its id and the answer.
type Running = BoxFuture<'static, Result<(CallId, ToolAnswer), String>>;

/// One dispatch's run on Rig's `AgentRun`, driven one [`step`](Self::step) at a
/// time so the caller stores what each step produced before the next one.
pub(crate) struct Run {
    setup: TurnSetup,
    pub(super) run: AgentRun,
    structured: Option<Structured>,
    reasks: usize,
    /// What this instance's built-in tools remember; handed back at the end.
    caches: Caches,
    connect: Connect,
    /// Connected from the first turn setup and kept for every turn (use_model behaviour).
    connected: Option<(DynModel<Completion>, Option<Thinking>)>,
    /// The last turn's tools, by name, and its sandbox, for the `CallTools`
    /// step after its model call.
    pinned: Option<(Arc<HashMap<String, Tool>>, Sandbox)>,
    /// How many of the run's messages earlier steps produced.
    produced: usize,
    /// The step taken after a model call, to run next.
    next: Option<AgentRunStep>,
    /// The tool calls of the pending batch, started at the next step.
    calls: FuturesUnordered<Running>,
}

impl Run {
    /// A run on `text` after `history`, each turn set up by `setup`, its built-in
    /// tools remembering in `caches`, its model connected by `connect`. With
    /// `structured`, the run must end with a successful `finish_structured` call.
    pub(crate) fn new(
        setup: TurnSetup,
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
            .max_turns(setup.max_turns);
        Self {
            setup,
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
                None => self.next_step()?,
            };
            match step {
                AgentRunStep::CallModel {
                    prompt, history, ..
                } => {
                    self.call_model(&prompt, &history).await?;
                    // B: the model's answer, once the run has taken it.
                    self.next = Some(self.next_step()?);
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
                        .ok_or("the run called tools before any turn setup")?;
                    // One reply's calls run concurrently; Rig commits them in call order.
                    // Each shares the turn's tools.
                    for call in calls {
                        let (tools, sandbox) = (Arc::clone(tools), sandbox.clone());
                        self.calls.push(Box::pin(
                            async move { answer(call, &tools, &sandbox).await },
                        ));
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

    fn next_step(&mut self) -> Result<AgentRunStep, String> {
        self.run.next_step().map_err(|e| match e {
            PromptError::MaxTurns { .. } => {
                format!("turn limit ({}) reached", self.setup.max_turns)
            }
            e => e.to_string(),
        })
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

    /// Sets up the turn and calls the model once, leaving its answer with the run.
    async fn call_model(
        &mut self,
        prompt: &RigMessage,
        history: &[Arc<RigMessage>],
    ) -> Result<(), String> {
        let max_turns = self.setup.max_turns;
        // The agent function runs again before every turn (ADR 0004).
        let declared = self.setup.run();
        let declared_model = declared.model()?;
        let sb = declared.sandbox()?.clone();
        let instructions = declared.instructions()?;
        let skills = declared.skills()?;
        let names = declared.builtin_names()?;
        let declared_tools = declared.tools()?;
        // Flue's order: the sandbox built-ins, the skill tools, then the declared ones.
        // The cache is taken only once the turn setup is valid, so a failed one keeps it.
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
                .insert(((self.connect)(declared_model)?, declared_model.thinking)),
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
            Arc::new(
                tools
                    .iter()
                    .map(|tool| (tool.name.to_string(), tool.clone()))
                    .collect(),
            ),
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
        // A call to a tool this turn did not declare is skipped with
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
