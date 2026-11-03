//! `finish_structured`: the built-in tool a dispatch that asks for a structured
//! result gets, and the messages the driver adds around it.

use std::marker::PhantomData;
use std::sync::{Arc, OnceLock};

use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use schemars::{JsonSchema, Schema};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::agent::rig::args_schema;
use crate::{Tool, ToolCall};

/// The reserved tool the model hands a structured result over with.
pub(crate) const FINISH_STRUCTURED: &str = "finish_structured";

/// Re-asks allowed after ending with text.
pub(crate) const MAX_STRUCTURED_REASKS: usize = 3;

/// What follows a message that asks for a structured result.
pub(crate) const FOOTER: &str = "When the task is complete, call `finish_structured` with your \
    answer as its arguments. The arguments are checked against its schema; if they don't match \
    you get an error and can try again. Don't answer in plain text: only a successful \
    `finish_structured` call counts.";

/// The message a re-ask sends.
pub(crate) const REASK: &str =
    "You ended without calling `finish_structured`. Call it now with your answer.";

/// What `finish_structured` tells the model, before `T`'s doc comment.
const FINISH_DESCRIPTION: &str = "Call this once, when the task is complete, with your final \
    answer as the arguments. They are validated against the schema; the first successful call wins.";

/// A `T` that is not a JSON object travels as `{"result": T}`.
#[derive(Deserialize, JsonSchema)]
struct Wrapped<T> {
    result: T,
}

// Validate as T but retain the original JSON, without requiring Serialize.
// Not rustdoc: schema derivation would expose this comment to the model.
#[derive(JsonSchema)]
#[schemars(transparent)]
struct Checked<T>(
    #[schemars(with = "T")] Value,
    #[schemars(skip)] PhantomData<fn() -> T>,
);

impl<'de, T: DeserializeOwned> Deserialize<'de> for Checked<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        T::deserialize(&value).map_err(D::Error::custom)?;
        Ok(Self(value, PhantomData))
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
    pub(crate) fn of<T>() -> Self
    where
        T: DeserializeOwned + JsonSchema + 'static,
    {
        let mut schema = result_schema::<T>();
        let description = match schemars::schema_for!(T).get("description") {
            Some(Value::String(doc)) => format!("{FINISH_DESCRIPTION}\n\n{doc}"),
            _ => FINISH_DESCRIPTION.to_owned(),
        };
        let recorded = Arc::default();
        // Keep the empty-object calling convention for `()`.
        if schema["type"] == "null" {
            schema = args_schema::<T>();
        }
        let mut tool = if schema["type"] == "object" {
            finish::<Checked<T>>(description, &recorded, |checked| checked.0)
        } else {
            schema = result_schema::<Wrapped<Checked<T>>>();
            finish::<Wrapped<Checked<T>>>(description, &recorded, |wrapped| wrapped.result.0)
        };
        schema.as_object_mut().unwrap().remove("description");
        // Ordinary tool arguments omit nulls; a structured result must retain them.
        tool.schema = Arc::new(schema);
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
                schema
                    .entry("additionalProperties")
                    .or_insert(Value::Bool(false));
            }
        }));
    settings.inline_subschemas = true;
    settings.meta_schema = None;
    settings
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

/// `finish_structured` over arguments `Args`, recording `unwrap(args)` in
/// `recorded`; the first call wins.
fn finish<Args>(
    description: String,
    recorded: &Arc<OnceLock<Value>>,
    unwrap: fn(Args) -> Value,
) -> Tool
where
    Args: DeserializeOwned + JsonSchema + Send + 'static,
{
    let recorded = recorded.clone();
    Tool::host(
        FINISH_STRUCTURED,
        description,
        move |call: ToolCall<Args>| async move {
            match recorded.set(unwrap(call.data)) {
                Ok(()) => Ok::<_, String>("Recorded."),
                Err(_) => Ok("already finished"),
            }
        },
    )
}
