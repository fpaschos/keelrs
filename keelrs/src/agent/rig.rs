//! The Rig integration: drives one Rig `AgentRun` per dispatch; the only module where Rig types appear (ADR 0004).
//!
//! One turn, no tools: a tool call fails the run.

use std::sync::Arc;

use rig_agent::run::{
    AgentRun, AgentRunStep, ModelTurn, ModelTurnOutcome, RunSpec, prepare_request,
};
use rig_core::catalog::Catalog;
use rig_core::completion::{CompletionRequest, Effort, GenerationOptions, Reasoning};
use rig_core::message::Message as RigMessage;

use crate::Agent;
use crate::model::Thinking;

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
    let mut run =
        AgentRun::new(RigMessage::user(text)).with_history(history.into_iter().map(|m| m.0));
    // Connected from the first render and kept for every turn (use_model behaviour).
    let mut connected = None;
    loop {
        match run.next_step().map_err(|e| e.to_string())? {
            AgentRunStep::CallModel {
                prompt, history, ..
            } => {
                // The agent function runs again before every turn (ADR 0004).
                let (cx, system) = agent.declare();
                let declared = cx.model()?;
                let (model, thinking) = match &mut connected {
                    Some(connected) => connected,
                    None => connected.insert((
                        Catalog::builtin()
                            .connect(declared.id.as_str())
                            .map_err(|e| e.to_string())?,
                        declared.thinking,
                    )),
                };

                let mut spec = RunSpec::new();
                spec.preamble = Some(system);
                if let Some(thinking) = thinking {
                    spec.options = GenerationOptions::default().reasoning(reasoning(*thinking));
                }
                let prepared = prepare_request(
                    &spec,
                    &model.capabilities().completion,
                    &history,
                    Vec::new(),
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;
                let policy = prepared.policy.clone();
                let response = model
                    .call(prepared.apply(CompletionRequest::new((*prompt).clone())))
                    .await
                    .map_err(|e| e.to_string())?;
                match run
                    .model_response(ModelTurn::from_policy(&response, policy))
                    .map_err(|e| e.to_string())?
                {
                    ModelTurnOutcome::Continue { .. } | ModelTurnOutcome::TurnRetried => {}
                    ModelTurnOutcome::NeedsResolution(_) => return Err(no_tools()),
                }
            }
            AgentRunStep::CallTools { .. } => return Err(no_tools()),
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

fn no_tools() -> String {
    "the model called a tool, but the agent has none".to_owned()
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
