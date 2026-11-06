//! The per-dispatch `finish_structured` tool and its driver messages.

use std::marker::PhantomData;
use std::sync::{Arc, OnceLock};

use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use schemars::{JsonSchema, Schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::rig::args_schema;
use crate::{Tool, ToolCall};

/// The reserved tool the model hands a structured result over with.
pub(crate) const FINISH_STRUCTURED: &str = "finish_structured";

/// Re-asks allowed after ending with text.
pub(crate) const MAX_STRUCTURED_REASKS: usize = 3;

pub(crate) const FOOTER: &str = "When the task is complete, call `finish_structured` with your \
    answer as its arguments. The arguments are checked against its schema; if they don't match \
    you get an error and can try again. Don't answer in plain text: only a successful \
    `finish_structured` call counts.";

pub(crate) const REASK: &str =
    "You ended without calling `finish_structured`. Call it now with your answer.";

const FINISH_DESCRIPTION: &str = "Call this once, when the task is complete, with your final \
    answer as the arguments. They are validated against the schema; the first successful call wins.";

/// A `T` that is not a JSON object travels as `{"result": T}`; only its schema is used.
#[derive(JsonSchema)]
#[expect(dead_code, reason = "only the schema is used")]
struct Wrapped<T> {
    result: Checked<T>,
}

// T's schema on a type that is never an `Option`, so `result` stays required.
// Not rustdoc: schema derivation would expose this comment to the model.
#[derive(JsonSchema)]
#[schemars(transparent)]
#[expect(dead_code, reason = "only the schema is used")]
struct Checked<T>(
    #[schemars(with = "T")] Value,
    #[schemars(skip)] PhantomData<fn() -> T>,
);

/// How `finish_structured`'s arguments carry the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Shape {
    /// They are the result, a JSON object.
    Object,
    /// As `{"result": T}`, `T` not being an object.
    Wrapped,
    /// An empty object stands for `()`, recorded as `null`.
    Unit,
}

/// What a structured result asks of the model: kept with the dispatch, so a
/// dispatch recovered from its events asks for the same result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StructuredSpec {
    /// The JSON schema of `finish_structured`'s arguments.
    schema: Value,
    description: String,
    shape: Shape,
}

impl StructuredSpec {
    /// The structured result `T`.
    pub(crate) fn of<T>() -> Self
    where
        T: DeserializeOwned + JsonSchema + 'static,
    {
        let mut schema = result_schema::<T>();
        let description = match schemars::schema_for!(T).get("description") {
            Some(Value::String(doc)) => format!("{FINISH_DESCRIPTION}\n\n{doc}"),
            _ => FINISH_DESCRIPTION.to_owned(),
        };
        let shape = if schema["type"] == "null" {
            // Keep the empty-object calling convention for `()`.
            schema = args_schema::<T>();
            Shape::Unit
        } else if schema["type"] == "object" {
            Shape::Object
        } else {
            schema = result_schema::<Wrapped<T>>();
            Shape::Wrapped
        };
        schema.as_object_mut().unwrap().remove("description");
        Self {
            schema,
            description,
            shape,
        }
    }
}

/// One dispatch's result tool and its first recorded value.
#[derive(Debug)]
pub(crate) struct Structured {
    pub(crate) tool: Tool,
    recorded: Arc<OnceLock<Value>>,
}

impl Structured {
    /// The structured result `T`.
    #[cfg(test)]
    pub(crate) fn of<T>() -> Self
    where
        T: DeserializeOwned + JsonSchema + 'static,
    {
        Self::from_spec(StructuredSpec::of::<T>())
    }

    /// The result `spec` asks for. The model's arguments are checked against
    /// its schema; the caller deserializes the recorded value.
    pub(crate) fn from_spec(spec: StructuredSpec) -> Self {
        let recorded: Arc<OnceLock<Value>> = Arc::default();
        let kept = recorded.clone();
        let shape = spec.shape;
        let mut tool = Tool::host(
            FINISH_STRUCTURED,
            spec.description,
            move |call: ToolCall<Value>| {
                let kept = kept.clone();
                async move {
                    let value = match (shape, call.data) {
                        (Shape::Wrapped, Value::Object(mut fields)) => {
                            fields.remove("result").unwrap_or(Value::Null)
                        }
                        (Shape::Unit, _) => Value::Null,
                        (_, value) => value,
                    };
                    match kept.set(value) {
                        Ok(()) => Ok::<_, String>("Recorded."),
                        Err(_) => Ok("already finished"),
                    }
                }
            },
        );
        // Ordinary tool arguments omit nulls; a structured result must retain them.
        tool.schema = Arc::new(spec.schema);
        Self { tool, recorded }
    }

    /// The value `finish_structured` recorded, if any.
    pub(crate) fn recorded(&self) -> Option<Value> {
        self.recorded.get().cloned()
    }
}

/// Rig 0.46.0-rc.1's plain schema formatting, without removing null alternatives.
fn result_schema<T: JsonSchema>() -> Value {
    let mut settings =
        SchemaSettings::draft2020_12().with_transform(RecursiveTransform(|schema: &mut Schema| {
            let Some(schema) = schema.as_object_mut() else {
                return;
            };
            for key in ["title", "default", "format"] {
                schema.remove(key);
            }
            if let Some(Value::String(text)) = schema.get_mut("description") {
                *text = text
                    .split("\n\n")
                    .map(|p| p.replace('\n', " "))
                    .collect::<Vec<_>>()
                    .join("\n\n");
            }
            if schema.contains_key("properties") {
                schema.entry("additionalProperties").or_insert(false.into());
            }
        }));
    settings.inline_subschemas = true;
    settings.meta_schema = None;
    settings
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}
