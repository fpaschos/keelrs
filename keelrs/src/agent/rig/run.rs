//! One dispatch's run on Rig's `AgentRun`: a plain future that persists what
//! it produced through a [`PersistSink`] before it goes on.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use rig_agent::run::{
    AgentRun, AgentRunStep, InvalidToolCallAction, ModelTurn, ModelTurnOutcome, PromptError,
    RetryRequest, RunSpec, TurnPolicy, prepare_request,
};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::message::turn_failure;
use rig_core::completion::{
    CompletionRequest, CompletionResponse, Effort, GenerationOptions, Reasoning, ToolDefinition,
};
use rig_core::message::{AssistantContent, Message as RigMessage, ToolName};
use rig_core::operation::Completion;

use super::call::{Answered, answer};
use super::history::Message;
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

/// Where a run stores what it produced before it goes on.
///
/// `persist` returns once `events` are stored. When they cannot be stored,
/// it never returns: the owner handles the failure and drops the run.
pub(crate) trait PersistSink: Send {
    async fn persist(&mut self, events: Vec<Event>);
}

/// A run that completed: its last events (stored with `settled`), its result
/// (a reply, or why it failed), and the caches for the next run.
#[derive(Debug)]
pub(crate) struct RunCompleted {
    pub(crate) events: Vec<Event>,
    pub(crate) result: Result<SettledValue, String>,
    pub(crate) caches: Caches,
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

/// A tool call running; it yields the answer and its result part.
type Running = BoxFuture<'static, Result<Answered, String>>;

/// One dispatch's run on Rig's `AgentRun`, driven to its end by [`drive`](Self::drive).
pub(crate) struct Run {
    setup: TurnSetup,
    pub(super) run: AgentRun,
    structured: Option<Structured>,
    reasks: usize,
    /// What this instance's built-in tools remember; handed back at the end.
    caches: Caches,
    connect: Connect,
    /// The declared model this run is connected to; connected again when a turn declares another.
    connected: Option<(Model, DynModel<Completion>)>,
    /// How many of the run's messages were produced so far.
    produced: usize,
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
            produced: 0,
        }
    }

    /// Runs the dispatch to its end, persisting each model answer (B) and
    /// each tool result (C) before it goes on.
    pub(crate) async fn drive(mut self, mut sink: impl PersistSink) -> RunCompleted {
        let (events, result) = match self.go(&mut sink).await {
            Ok((events, value)) => (events, Ok(value)),
            // The owner adds the closing events.
            Err(reason) => (Vec::new(), Err(reason)),
        };
        RunCompleted {
            events,
            result,
            caches: self.caches,
        }
    }

    /// The loop. Returns D: the last events and the settled value; an error
    /// is the failure reason, as plain text.
    async fn go(
        &mut self,
        sink: &mut impl PersistSink,
    ) -> Result<(Vec<Event>, SettledValue), String> {
        // The last turn's tools, by name, and its sandbox.
        let mut pinned = None;
        let mut step = self.next_step()?;
        loop {
            step = match step {
                AgentRunStep::CallModel {
                    prompt, history, ..
                } => {
                    pinned = Some(self.call_model(&prompt, &history).await?);
                    let next = self.next_step()?;
                    // B, unless the answer is final: then it goes with D.
                    if !matches!(next, AgentRunStep::Done(_)) {
                        let events = self.produced();
                        if !events.is_empty() {
                            sink.persist(events).await;
                        }
                    }
                    next
                }
                AgentRunStep::CallTools { calls } => {
                    let (tools, sandbox) = pinned
                        .as_ref()
                        .ok_or("the run called tools before any turn setup")?;
                    // One reply's calls run concurrently; Rig commits them in call order.
                    // Each shares the turn's tools.
                    let mut running: FuturesUnordered<Running> = calls
                        .into_iter()
                        .map(|call| {
                            let (tools, sandbox) = (Arc::clone(tools), sandbox.clone());
                            Box::pin(async move { answer(call, &tools, &sandbox).await }) as Running
                        })
                        .collect();
                    while let Some(answered) = running.next().await {
                        let (answer, part) = answered?;
                        self.run.answer(answer).map_err(|e| e.to_string())?;
                        let event = Event::ToolResult(part);
                        if running.is_empty() {
                            // The batch is committed: Rig's history now holds its results as one
                            // message. Its own event is decided in issue #47.
                            self.produced = self.run.messages().len();
                            // A recorded result ends the dispatch once its whole batch
                            // has run: the last result goes with D.
                            if let Some(value) =
                                self.structured.as_ref().and_then(Structured::recorded)
                            {
                                let value = SettledValue {
                                    text: String::new(),
                                    structured: Some(value),
                                };
                                return Ok((vec![event], value));
                            }
                        }
                        // C
                        sink.persist(vec![event]).await;
                    }
                    self.next_step()?
                }
                AgentRunStep::Done(response) => {
                    let value = SettledValue {
                        text: response.output(),
                        structured: None,
                    };
                    // D: the final answer and settled together.
                    return Ok((self.produced(), value));
                }
            };
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

    /// The run's messages not produced yet, as events.
    fn produced(&mut self) -> Vec<Event> {
        let messages = &self.run.messages()[self.produced..];
        self.produced += messages.len();
        messages
            .iter()
            .map(|m| Event::Message(Message(m.clone())))
            .collect()
    }

    /// Sets up the turn and calls the model once, leaving its answer with the
    /// run. Returns the turn's tools, by name, and its sandbox.
    async fn call_model(
        &mut self,
        prompt: &RigMessage,
        history: &[Arc<RigMessage>],
    ) -> Result<(Arc<HashMap<String, Tool>>, Sandbox), String> {
        let turn = self.set_up_turn()?;
        let model = self.connect(&turn.model)?;
        let (response, policy) = call(model, &turn, prompt, history).await?;
        self.accept(&response, policy)?;
        self.reask_if_text(&response)?;
        Ok((turn.tools, turn.sandbox))
    }

    /// Runs the agent function again (ADR 0004) and builds the turn it declared.
    fn set_up_turn(&mut self) -> Result<Turn, String> {
        let declared = self.setup.run();
        let model = declared.model()?.clone();
        let sandbox = declared.sandbox()?.clone();
        let instructions = declared.instructions()?;
        let skills = declared.skills()?;
        let names = declared.builtin_names()?;
        let declared_tools = declared.tools()?;
        // Flue's order: the sandbox built-ins, the skill tools, then the declared ones.
        // The cache is taken only once the turn setup is valid, so a failed one keeps it.
        let tools: Vec<Tool> = builtin::tools(&sandbox, &self.caches.seen(sandbox.id()), &names)
            .into_iter()
            .chain(skill_tools(skills))
            .chain(declared_tools.iter().cloned())
            .chain(self.structured.as_ref().map(|s| s.tool.clone()))
            .collect();
        let definitions = definitions(&tools)?;
        let mut spec = RunSpec::new();
        // The skill catalog follows the instructions (How a skill reaches the model).
        let parts: Vec<String> = instructions.into_iter().chain(catalog(skills)).collect();
        spec.preamble = (!parts.is_empty()).then(|| parts.join("\n\n"));
        if let Some(thinking) = model.thinking {
            spec.options = GenerationOptions::default().reasoning(reasoning(thinking));
        }
        let tools = Arc::new(tools.into_iter().map(|t| (t.name.to_string(), t)).collect());
        Ok(Turn {
            model,
            spec,
            definitions,
            tools,
            sandbox,
        })
    }

    /// The connection to `declared`; connected again when the last turn declared another model.
    fn connect(&mut self, declared: &Model) -> Result<&DynModel<Completion>, String> {
        if self.connected.as_ref().is_none_or(|(m, _)| m != declared) {
            self.connected = Some((declared.clone(), (self.connect)(declared)?));
        }
        Ok(&self.connected.as_ref().expect("set above").1)
    }

    /// Gives the run the model's answer. A call to a tool this turn did not
    /// declare is skipped with the names it did; its siblings are re-issued next turn.
    fn accept(&mut self, response: &CompletionResponse, policy: TurnPolicy) -> Result<(), String> {
        let run = &mut self.run;
        let mut outcome = run
            .model_response(ModelTurn::from_policy(response, policy))
            .map_err(|e| e.to_string())?;
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
        Ok(())
    }

    /// A text ending, when a structured result is asked for, is re-asked
    /// in the same run: the reply stays, the re-ask follows it.
    fn reask_if_text(&mut self, response: &CompletionResponse) -> Result<(), String> {
        let failed = turn_failure(
            &response.choice,
            response.head().stop.as_ref(),
            response.finish_reason().as_ref(),
        )
        .is_some();
        let called = response
            .choice
            .iter()
            .any(|item| matches!(item, AssistantContent::ToolCall(_)));
        if self.structured.is_none() || failed || called {
            return Ok(());
        }
        let max_turns = self.setup.max_turns;
        if self.reasks == MAX_STRUCTURED_REASKS {
            return Err(format!(
                "ended without calling `{FINISH_STRUCTURED}` after {} re-asks",
                self.reasks
            ));
        }
        if self.run.turn() >= max_turns {
            return Err(format!(
                "ended without calling `{FINISH_STRUCTURED}`: turn limit ({max_turns}) reached"
            ));
        }
        self.reasks += 1;
        self.run
            .retry_model_turn(RetryRequest::Feedback(REASK.to_owned()))
            .map_err(|e| e.to_string())
    }
}

/// One turn's setup, from the agent function's declaration.
struct Turn {
    model: Model,
    /// The instructions, then the skill catalog, and the thinking level.
    spec: RunSpec,
    definitions: Vec<Arc<ToolDefinition>>,
    /// The turn's tools, by name, pinned for the `CallTools` step that follows.
    tools: Arc<HashMap<String, Tool>>,
    sandbox: Sandbox,
}

/// What the model is sent for each of `tools`.
fn definitions(tools: &[Tool]) -> Result<Vec<Arc<ToolDefinition>>, String> {
    tools
        .iter()
        .map(|tool| {
            let name = ToolName::new(&*tool.name).map_err(|e| e.to_string())?;
            let (description, schema) = (&*tool.description, (*tool.schema).clone());
            Ok(Arc::new(ToolDefinition::new(name, description, schema)))
        })
        .collect()
}

/// Calls `model` once with `turn`'s request; returns its answer and the
/// policy the run reads it by.
async fn call(
    model: &DynModel<Completion>,
    turn: &Turn,
    prompt: &RigMessage,
    history: &[Arc<RigMessage>],
) -> Result<(CompletionResponse, TurnPolicy), String> {
    let prepared = prepare_request(
        &turn.spec,
        &model.capabilities().completion,
        history,
        turn.definitions.clone(),
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    let policy = prepared.policy.clone();
    let response = model
        .call(prepared.apply(CompletionRequest::new(prompt.clone())))
        .await
        .map_err(|e| e.to_string())?;
    Ok((response, policy))
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
