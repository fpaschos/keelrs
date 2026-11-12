//! A scripted model, so agent instances are tested through a [`Runtime`]
//! without a network: give it to [`RuntimeBuilder::model`] and every
//! instance of that runtime uses it in place of the model it declares.
//!
//! [`Runtime`]: crate::Runtime
//! [`RuntimeBuilder::model`]: crate::RuntimeBuilder::model

use serde_json::Value;

use crate::agent::rig::{Connect, Script};

/// A model that answers each turn from a script, in order, and keeps what
/// each turn sent it. Clones share the script and the requests.
///
/// A turn past the end of the script fails the run.
#[derive(Debug, Clone, Default)]
pub struct ScriptedModel {
    script: Script,
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
        self.script.reply(text.into());
        self
    }

    /// Adds a turn that calls each `(id, tool, arguments)`, in order.
    ///
    /// # Panics
    ///
    /// When a tool name is not one a model can call (empty, for example).
    #[must_use]
    pub fn calls<const N: usize>(self, calls: [(&str, &str, Value); N]) -> Self {
        self.script.calls(calls);
        self
    }

    /// Adds a turn that never ends, for a test that aborts a live step.
    #[must_use]
    pub fn hang(self) -> Self {
        self.script.hang();
        self
    }

    /// What each turn so far sent the model, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.script.requests()
    }

    /// Connects every declared model to this script.
    pub(crate) fn connect(&self) -> Connect {
        self.script.connect()
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
