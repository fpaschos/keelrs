//! The Rig side of [`ScriptedModel`](crate::testing::ScriptedModel): a
//! transport over rig-core's `MockScript` that answers each turn from a
//! script and keeps what each turn sent.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use rig_core::completion::CompletionRequest;
use rig_core::driver::{Exchange, Model, Opening, Transport};
use rig_core::error::ProviderError;
use rig_core::message::{
    AssistantContent, Message as RigMessage, ToolCall, ToolFunction, ToolName, ToolResultContent,
    UserContent,
};
use rig_core::test_utils::{MockCompletionModel, MockFrame, MockScript, MockTurn};
use serde_json::Value;

use super::Connect;
use crate::testing::Request;

/// A script of turns and the requests they got; clones share both.
#[derive(Debug, Clone, Default)]
pub(crate) struct Script(Arc<Mutex<State>>);

#[derive(Debug, Default)]
struct State {
    turns: VecDeque<Turn>,
    /// What each turn sent, as plain data.
    requests: Vec<Request>,
}

#[derive(Debug)]
enum Turn {
    Reply(Box<MockTurn>),
    /// A turn that never ends.
    Hang,
}

impl Script {
    /// Adds a turn that replies with `text`.
    pub(crate) fn reply(&self, text: String) {
        self.then(Turn::Reply(Box::new(MockTurn::text(text))));
    }

    /// Adds a turn that calls each `(id, tool, arguments)`, in order; panics
    /// on a tool name a model cannot call.
    pub(crate) fn calls<const N: usize>(&self, calls: [(&str, &str, Value); N]) {
        let contents = calls.map(|(id, tool, arguments)| {
            let name = ToolName::new(tool).expect("a tool name a model can call");
            AssistantContent::ToolCall(ToolCall::from_wire(id, ToolFunction::new(name, arguments)))
        });
        self.then(Turn::Reply(Box::new(MockTurn::from_contents(contents))));
    }

    /// Adds a turn that never ends.
    pub(crate) fn hang(&self) {
        self.then(Turn::Hang);
    }

    /// What each turn so far sent the model, in order.
    pub(crate) fn requests(&self) -> Vec<Request> {
        self.0.lock().requests.clone()
    }

    /// Connects every declared model to this script.
    pub(crate) fn connect(&self) -> Connect {
        let transport = self.clone();
        Arc::new(move |_| Ok(Model::new(MockScript::default(), transport.clone()).erase()))
    }

    fn then(&self, turn: Turn) {
        self.0.lock().turns.push_back(turn);
    }
}

impl Transport<MockScript> for Script {
    fn send(&self, request: CompletionRequest, exchange: Exchange) -> Opening<MockFrame> {
        let turn = {
            let mut state = self.0.lock();
            state.requests.push(self::request(&request));
            state.turns.pop_front()
        };
        match turn {
            None => Opening::failed(ProviderError::Provider(
                "the scripted model has no turn left".to_owned(),
            )),
            Some(Turn::Hang) => Opening::new(std::future::pending()),
            // rig-core's runtime answers one turn; it takes no turns once built.
            Some(Turn::Reply(turn)) => MockCompletionModel::from_turns([*turn])
                .transport
                .send(request, exchange),
        }
    }
}

/// What `request` sent the model, as plain data.
fn request(request: &CompletionRequest) -> Request {
    let mut instructions = None;
    let mut messages = Vec::new();
    for message in &request.chat_history {
        match message.as_ref() {
            RigMessage::System { content } => instructions = Some(content.clone()),
            RigMessage::User { content } => messages.extend(content.iter().map(user_part)),
            RigMessage::Assistant(reply) => {
                messages.extend(reply.content.iter().map(assistant_part));
            }
        }
    }
    // A definition serializes as `{name, description, parameters}`.
    let tools = request
        .tools
        .iter()
        .map(|tool| serde_json::to_value(&**tool).expect("a tool definition serializes"))
        .collect();
    Request {
        instructions,
        messages,
        tools,
    }
}

fn user_part(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.text.clone(),
        UserContent::ToolResult(result) => {
            let text: String = result
                .content
                .iter()
                .map(|c| match c {
                    ToolResultContent::Text(t) => t.text.clone(),
                    ToolResultContent::Json { value } => value.to_string(),
                    image @ ToolResultContent::Image(_) => format!("{image:?}"),
                })
                .collect();
            let kind = if result.is_error { "error" } else { "result" };
            format!("{kind}:{text}")
        }
        other => format!("{other:?}"),
    }
}

fn assistant_part(content: &AssistantContent) -> String {
    match content {
        AssistantContent::Text(text) => text.text.clone(),
        AssistantContent::ToolCall(call) => format!("call:{}", call.function.name),
        other => format!("{other:?}"),
    }
}
