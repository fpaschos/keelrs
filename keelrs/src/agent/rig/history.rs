//! The messages a run accepted, as the stream stores them, and how tool
//! results join the history.

use std::sync::Arc;

use rig_core::conversation::pairing;
use rig_core::message::{
    AssistantContent, CallId, Message as RigMessage, ToolResult as RigToolResult, UserContent,
};
use serde::{Deserialize, Serialize};

use crate::event::Event;

/// One message a run accepted; opaque so Rig's type stays in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Message(pub(super) Arc<RigMessage>);

/// One tool call's result, as the next user message carries it; opaque so
/// Rig's type stays in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ToolResultPart(pub(super) RigToolResult);

/// Adds a run's message or tool result to `history`, as the next run reads
/// it; any other event leaves it unchanged.
pub(crate) fn record(history: &mut Vec<Message>, event: Event) {
    match event {
        Event::Message(message) => history.push(message),
        Event::ToolResult(part) => push_result(history, &part),
        _ => {}
    }
}

/// Adds `part` to `history`: into the tool results that follow the last
/// assistant message, kept in its call order, or as a new user message.
fn push_result(history: &mut Vec<Message>, part: &ToolResultPart) {
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
