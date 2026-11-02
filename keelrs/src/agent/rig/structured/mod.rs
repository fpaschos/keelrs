//! Structured results: the reserved `finish_structured` tool a dispatch asks
//! for a typed value with, and the messages the driver adds around it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::args_schema;
use crate::Tool;
use crate::sandbox::Sandbox;
use crate::tool::{Called, FailKind, Failed};

/// The reserved tool the model hands a structured result over with.
pub(crate) const FINISH_STRUCTURED: &str = "finish_structured";

/// The structured result one dispatch asks for: `finish_structured`'s schema and
/// description, and a check that a value deserializes into `T`.
#[derive(Debug, Clone)]
pub(crate) struct Structured {
    /// `finish_structured`'s parameters: `T`'s schema, wrapped as `{result}` when not an object.
    schema: Value,
    /// Whether the value travels as `{"result": value}`.
    wrapped: bool,
    /// `finish_structured`'s description, ending with `T`'s doc comment.
    description: String,
    /// Deserializes a value into `T`; the error is what the model reads.
    check: fn(Value) -> Result<(), String>,
}

impl Structured {
    /// The structured result `T`.
    pub(crate) fn of<T: DeserializeOwned + JsonSchema>() -> Self {
        // `args_schema` drops the root doc comment and inlines every subschema.
        let description = match schemars::schema_for!(T).get("description") {
            Some(Value::String(doc)) => format!("{FINISH_DESCRIPTION}\n\n{doc}"),
            _ => FINISH_DESCRIPTION.to_owned(),
        };
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
            description,
            check: |value| {
                serde_json::from_value::<T>(value)
                    .map(drop)
                    .map_err(|e| e.to_string())
            },
        }
    }

    /// The `finish_structured` tool, recording the first value that fits in `recorded`.
    pub(super) fn tool(&self, recorded: Arc<Mutex<Option<Value>>>) -> Tool {
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
            description: self.description.as_str().into(),
            schema: Arc::new(self.schema.clone()),
            timeout: None,
            run: Arc::new(run),
        }
    }
}

/// How many times a dispatch that asked for a structured result is asked again
/// after ending with text.
pub(super) const MAX_STRUCTURED_REASKS: usize = 3;

/// What follows a message that asks for a structured result.
pub(super) const FOOTER: &str = "When the task is complete, call `finish_structured` with your answer as \
    its arguments. The arguments are checked against its schema; if they don't match you get an \
    error and can try again. Don't answer in plain text: only a successful `finish_structured` \
    call counts.";

/// The message a re-ask sends.
pub(super) const REASK: &str =
    "You ended without calling `finish_structured`. Call it now with your answer.";

/// What `finish_structured` tells the model, before `T`'s doc comment.
const FINISH_DESCRIPTION: &str = "Call this once, when the task is complete, with your final \
    answer as the arguments. They are validated against the schema; the first successful call wins.";

#[cfg(test)]
mod tests;
