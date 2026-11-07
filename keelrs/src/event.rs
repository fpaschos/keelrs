//! keelrs's canonical events: the source of truth for an agent instance (ADR 0004).
//!
//! Private: a store keeps each one as its `type` and its fields as JSON
//! ([`StoredEvent`]), so the catalog can change without breaking stores.
//! The names are provisional until the agent instance's event catalog.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Outcome;
use crate::agent::SettledValue;
use crate::agent::rig::{Message, ToolResultPart};
use crate::builtin::structured::StructuredSpec;
use crate::store::StoredEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "event", rename_all = "snake_case")]
pub(crate) enum Event {
    /// The instance began, with its data as JSON: always its stream's first event.
    Created { data: Value },
    /// A message was dispatched: kept before its receipt is returned.
    Dispatched {
        text: String,
        /// The structured result it asks for, if any.
        structured: Option<StructuredSpec>,
    },
    /// A message the run accepted (the user's prompt or the assistant's turn).
    Message(Message),
    /// One tool call's result, kept as it finishes.
    ToolResult(ToolResultPart),
    /// The dispatched message settled: its reply text and structured result, or how it failed.
    Settled {
        result: Result<SettledValue, Outcome>,
    },
}

impl Event {
    /// The event's `type` and its fields, as a [`StoredEvent`] holds them.
    pub(crate) fn to_parts(&self) -> (String, Value) {
        let Ok(Value::Object(mut tagged)) = serde_json::to_value(self) else {
            unreachable!("an adjacently tagged enum serializes as an object");
        };
        let Some(Value::String(kind)) = tagged.remove("type") else {
            unreachable!("every event has a type");
        };
        (kind, tagged.remove("event").unwrap_or(Value::Null))
    }

    /// The typed event a store gave back.
    pub(crate) fn of(stored: &StoredEvent) -> Result<Self, String> {
        let tagged = serde_json::json!({ "type": stored.kind, "event": stored.event });
        serde_json::from_value(tagged)
            .map_err(|e| format!("event {} (`{}`): {e}", stored.seq, stored.kind))
    }
}
