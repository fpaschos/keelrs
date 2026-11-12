//! The tool loop, driven on rig-core's scripted model: no network; and one
//! call through [`call`], the seam the loop runs each call with. Shared
//! helpers here, the tests by topic in the child modules.
//!
//! Unit tests, not `keelrs/tests/`: they inject the model through the private
//! `Run::with_connect` and read Rig's messages from `Settled`, neither of which the
//! public API exposes (Rig types stay in this module, ADR 0004).

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rig_core::completion::AssistantContent;
use rig_core::message::{ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Barrier;

use std::sync::Arc;

use rig_core::completion::CompletionRequest;
use rig_core::message::{Message as RigMessage, ToolFunction, ToolName};
use rig_core::tool::ToolErrorKind;
use serde_json::Value;

use super::call::result;
use super::*;
use crate::Tool;
use crate::agent::TurnSetup;
use crate::builtin::Caches;
use crate::builtin::structured::{FINISH_STRUCTURED, Structured};
use crate::event::Event;
use crate::sandbox::{Sandbox, unused};
use crate::test_support::{Folder, haiku, sonnet};
use crate::tool::{FailKind, Failed};
use crate::{Agent, AgentContext, InstanceId, Skill, ToolCall};

mod builtin;
mod call;
mod locks;
mod skills;
mod structured;
mod turns;

/// Runs `agent` on one message against a model that replies with `turns`.
async fn scripted(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> Result<Settled, String> {
    scripted_in(agent, turns, &mut Caches::default()).await
}

/// [`scripted`], as the agent instance whose built-in tools remember in `caches`.
async fn scripted_in(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
    caches: &mut Caches,
) -> Result<Settled, String> {
    drive(agent, turns, None, caches).await.0
}

/// Runs `agent` on "Go", asking for `structured`, against a model that replies
/// with `turns`; also every request the model got.
async fn drive(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
    structured: Option<Structured>,
    caches: &mut Caches,
) -> (Result<Settled, String>, Vec<CompletionRequest>) {
    let (run, model) = scripted_run(
        agent,
        "Go",
        Vec::new(),
        structured,
        std::mem::take(caches),
        turns,
    );
    let (run, settled) = settle(run).await;
    *caches = run.into_caches();
    (settled, model.requests())
}

/// A run of `agent` on `text` after `history`, against a model that replies
/// with `turns`; and that model, which keeps every request it got.
pub(crate) fn scripted_run(
    agent: &Agent,
    text: &str,
    history: Vec<Message>,
    structured: Option<Structured>,
    caches: Caches,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Run, MockCompletionModel) {
    let model = MockCompletionModel::from_turns(turns);
    let mock = model.clone();
    let run = Run::with_connect(
        TurnSetup::new(agent, InstanceId::new("test"), ()),
        text.to_owned(),
        history,
        structured,
        caches,
        Box::new(move |_| Ok(mock.clone().erase())),
    );
    (run, model)
}

/// A run that settled with a reply, as its steps produced it.
#[derive(Debug)]
pub(crate) struct Settled {
    /// The final assistant text.
    pub(crate) reply: String,
    /// What the run accepted, prompt first, excluding the input history.
    pub(crate) messages: Vec<Message>,
    /// The structured result `finish_structured` recorded, when one was asked for.
    pub(crate) structured: Option<Value>,
}

/// Steps `run` to settled, folding each step's events with [`record`], as
/// the instance does.
pub(crate) async fn settle(mut run: Run) -> (Run, Result<Settled, String>) {
    let mut messages = Vec::new();
    loop {
        let (next, step) = run.step().await;
        run = next;
        match step {
            Ok(Step::Append(events)) => fold(&mut messages, events),
            Ok(Step::Settled(events, value)) => {
                fold(&mut messages, events);
                // What the steps produced folds back to what the run itself committed.
                let folded: Vec<&RigMessage> = messages.iter().map(|m| m.0.as_ref()).collect();
                let committed: Vec<&RigMessage> =
                    run.run.messages().iter().map(AsRef::as_ref).collect();
                assert_eq!(folded, committed, "the stored history differs from Rig's");
                let settled = Settled {
                    reply: value.text,
                    messages,
                    structured: value.structured,
                };
                return (run, Ok(settled));
            }
            Err(reason) => return (run, Err(reason)),
        }
    }
}

/// Adds a step's events to `history`; a run produces only messages and tool results.
fn fold(history: &mut Vec<Message>, events: Vec<Event>) {
    for event in events {
        assert!(
            matches!(event, Event::Message(_) | Event::ToolResult(_)),
            "a run produced {event:?}"
        );
        record(history, event);
    }
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

/// A local sandbox on a fresh temp folder, which is removed on drop.
async fn sandboxed() -> (Folder, Sandbox) {
    let folder = Folder::new();
    let sb = crate::sandbox::local()
        .root(&folder.0)
        .open()
        .await
        .unwrap();
    (folder, sb)
}

/// An agent on the mock model whose built-ins act through `sb`.
fn agent_in(sb: &Sandbox) -> Agent {
    let sb = sb.clone();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb);
    })
}

/// An agent on the mock model, in an unused sandbox with no built-ins.
pub(crate) fn agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    let sb = unused();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only([]);
        declare(cx);
    })
}

/// An agent on the mock model whose `declare` calls `use_sandbox` itself.
fn sandboxed_agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    Agent::new("scripted", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        declare(cx);
        String::new()
    })
}

pub(crate) fn clock() -> Tool {
    Tool::host(
        "current_time",
        "The current time.",
        async |_: ToolCall<()>| Ok::<_, Infallible>("noon"),
    )
}

#[derive(Deserialize, JsonSchema)]
struct Order {
    order_id: String,
}

fn lookup_order() -> Tool {
    Tool::host(
        "lookup_order",
        "Look up an order.",
        async |call: ToolCall<Order>| {
            Ok::<_, Infallible>(json!({"order": call.data.order_id, "call": call.id}))
        },
    )
}

/// Runs `agent` like [`scripted`] and returns every request the model got.
async fn sent(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Settled, Vec<CompletionRequest>) {
    let (settled, requests) = drive(agent, turns, None, &mut Caches::default()).await;
    (settled.unwrap(), requests)
}

/// One reply calling each `(id, tool, arguments)` in order.
fn reply(calls: impl IntoIterator<Item = (&'static str, &'static str, Value)>) -> MockTurn {
    MockTurn::from_contents(calls.into_iter().map(|(id, name, arguments)| {
        let function = ToolFunction::new(ToolName::new(name).unwrap(), arguments);
        AssistantContent::ToolCall(rig_core::message::ToolCall::from_wire(id, function))
    }))
}

/// The preamble a request carried.
fn preamble(request: &CompletionRequest) -> Option<&str> {
    match request.chat_history.first()?.as_ref() {
        RigMessage::System { content } => Some(content),
        _ => None,
    }
}

/// The names of the tools a request declared.
fn tool_names(request: &CompletionRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}
