//! A scripted model, so agent instances are tested through a [`Runtime`]
//! without a network: give it to [`RuntimeBuilder::model`] and every
//! instance of that runtime uses it in place of the model it declares.
//!
//! [`Runtime`]: crate::Runtime
//! [`RuntimeBuilder::model`]: crate::RuntimeBuilder::model

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::driver::{Exchange, Model, Opened, Opening, Transport};
use rig_core::error::ProviderError;
use rig_core::message::{
    AssistantContent, Message as RigMessage, ToolCall, ToolFunction, ToolName, ToolResultContent,
    UserContent,
};
use rig_core::test_utils::{MockCompletionModel, MockFrame, MockScript, MockTurn};
use serde_json::{Value, json};

use crate::agent::rig::Connect;

/// A model that answers each turn from a script, in order, and keeps what
/// each turn sent it. Clones share the script and the requests.
///
/// A turn past the end of the script fails the run.
#[derive(Debug, Clone, Default)]
pub struct ScriptedModel {
    script: Arc<Mutex<Script>>,
}

#[derive(Debug, Default)]
struct Script {
    turns: VecDeque<Turn>,
    requests: Vec<CompletionRequest>,
}

#[derive(Debug)]
enum Turn {
    Reply(Box<MockTurn>),
    /// A turn that never ends.
    Hang,
}

impl ScriptedModel {
    /// A model with an empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a turn that replies with `text`.
    #[must_use]
    pub fn reply(self, text: impl Into<String>) -> Self {
        self.then(Turn::Reply(Box::new(MockTurn::text(text))))
    }

    /// Adds a turn that calls each `(id, tool, arguments)`, in order.
    ///
    /// # Panics
    ///
    /// When a tool name is not one a model can call (empty, for example).
    #[must_use]
    pub fn calls<const N: usize>(self, calls: [(&str, &str, Value); N]) -> Self {
        let contents = calls.map(|(id, tool, arguments)| {
            let name = ToolName::new(tool).expect("a tool name a model can call");
            AssistantContent::ToolCall(ToolCall::from_wire(id, ToolFunction::new(name, arguments)))
        });
        self.then(Turn::Reply(Box::new(MockTurn::from_contents(contents))))
    }

    /// Adds a turn that never ends, for a test that aborts a live step.
    #[must_use]
    pub fn hang(self) -> Self {
        self.then(Turn::Hang)
    }

    /// What each turn so far sent the model, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.script
            .lock()
            .requests
            .iter()
            .map(Request::of)
            .collect()
    }

    fn then(self, turn: Turn) -> Self {
        self.script.lock().turns.push_back(turn);
        self
    }

    /// Connects every declared model to this script.
    pub(crate) fn connect(&self) -> Connect {
        let transport = Scripted(Arc::clone(&self.script));
        Arc::new(move |_| Ok(Model::new(MockScript::default(), transport.clone()).erase()))
    }
}

/// What one turn sent the model, as plain data.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Request {
    /// The preamble: the instructions, then the skill catalog.
    pub instructions: Option<String>,
    /// The history, then the prompt, one entry per part: a text as itself,
    /// a tool call as `call:<tool>`, a tool result as `result:<text>`, and a
    /// failed one as `error:<text>`.
    pub messages: Vec<String>,
    /// Each tool, in order, as `{"name", "description", "parameters"}`.
    pub tools: Vec<Value>,
}

impl Request {
    fn of(request: &CompletionRequest) -> Self {
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
        let tools = request.tools.iter().map(|tool| tool_json(tool)).collect();
        Self {
            instructions,
            messages,
            tools,
        }
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

fn tool_json(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name.to_string(),
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

/// The transport behind a [`ScriptedModel`]: answers each send with the
/// script's next turn.
#[derive(Clone)]
struct Scripted(Arc<Mutex<Script>>);

impl Transport<MockScript> for Scripted {
    fn send(&self, request: CompletionRequest, _: Exchange) -> Opening<MockFrame> {
        let turn = {
            let mut script = self.0.lock();
            script.requests.push(request.clone());
            script.turns.pop_front()
        };
        match turn {
            None => Opening::failed(ProviderError::Provider(
                "the scripted model has no turn left".to_owned(),
            )),
            Some(Turn::Hang) => Opening::new(std::future::pending()),
            // rig-core's own scripted model answers one turn.
            Some(Turn::Reply(turn)) => Opening::new(async move {
                let response = MockCompletionModel::from_turns([*turn])
                    .call(request)
                    .await?;
                let document = response.raw.clone();
                let frame = Ok(MockFrame::Response(Box::new(response)));
                Ok(Opened::new(futures::stream::iter([frame])).with_document(document))
            }),
        }
    }
}
