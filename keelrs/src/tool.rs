//! Tools an agent declares with [`AgentContext::use_tool`](crate::AgentContext::use_tool).
//!
//! A [`Tool`] is plain keelrs data around an erased callback; the Rig driver
//! (`agent::rig`) turns it into Rig's tool definition and result each turn.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// What one call of a tool receives: the model's arguments and the call's id.
#[derive(Debug)]
#[non_exhaustive]
pub struct ToolCall<Args> {
    /// The arguments, deserialised from what the model sent.
    pub data: Args,
    /// The provider's id for this call.
    pub id: String,
}

/// What a tool's call settles to: its value, or the message the model reads.
type Called = Result<Value, String>;

type Run = dyn Fn(Value, String) -> Pin<Box<dyn Future<Output = Called> + Send>> + Send + Sync;

/// A named function the model may call during a turn, declared with
/// [`AgentContext::use_tool`](crate::AgentContext::use_tool).
///
/// Cheap to clone: an agent function may build it every turn or keep one.
#[derive(Clone)]
pub struct Tool {
    pub(crate) name: Arc<str>,
    pub(crate) description: Arc<str>,
    /// The JSON schema of the arguments, sent to the model.
    pub(crate) schema: Arc<Value>,
    pub(crate) timeout: Option<Duration>,
    /// Runs one call with the model's arguments and the call's id.
    pub(crate) run: Arc<Run>,
}

impl Tool {
    /// A tool named `name` that the model reads `description` of and calls with
    /// arguments of type `Args`, running `run`.
    ///
    /// `Args`'s schema (schemars, field docs become descriptions) must be a
    /// JSON object; `ToolCall<()>` takes no arguments. `run` is cloned before
    /// each call, so `async move |call| …` closures over `Arc`s work. An `Ok`
    /// string reaches the model as text and any other value as JSON; an `Err`
    /// reaches it as its message, and the run continues.
    ///
    /// A call is cancelled by dropping its future (a timeout, or the instance
    /// stopping): work a tool spawns outside that future needs a drop guard.
    pub fn new<Args, F, Fut, T, E>(
        name: impl Into<String>,
        description: impl Into<String>,
        run: F,
    ) -> Self
    where
        Args: DeserializeOwned + JsonSchema + Send + 'static,
        F: FnOnce(ToolCall<Args>) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        T: Serialize + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let name: Arc<str> = name.into().into();
        let tool_name = name.clone();
        let run = move |arguments: Value, id: String| {
            let run = run.clone();
            let name = tool_name.clone();
            Box::pin(async move {
                let data = arguments_into::<Args>(arguments)
                    .map_err(|e| format!("`{name}`'s arguments do not fit: {e}"))?;
                let value = run(ToolCall { data, id })
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(value).map_err(|e| e.to_string())
            }) as Pin<Box<dyn Future<Output = Called> + Send>>
        };
        Self {
            name,
            description: description.into().into(),
            schema: Arc::new(crate::agent::rig::args_schema::<Args>()),
            timeout: None,
            run: Arc::new(run),
        }
    }

    /// Stops a call after `timeout`; the model then reads that it timed out.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// `arguments` as `Args`; an empty object also reads as `()`, which serde
/// only takes from `null`.
fn arguments_into<Args: DeserializeOwned>(arguments: Value) -> serde_json::Result<Args> {
    let empty = arguments.as_object().is_some_and(serde_json::Map::is_empty);
    serde_json::from_value(arguments).or_else(|e| {
        if empty {
            serde_json::from_value(Value::Null).map_err(|_| e)
        } else {
            Err(e)
        }
    })
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}
