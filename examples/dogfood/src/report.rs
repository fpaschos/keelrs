//! Structured results: an agent ends by calling a report tool, whose arguments the flow takes.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use keelrs::{AgentInstance, Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

/// What an agent reports; the instructions tell it to call this tool last.
pub struct Report<T> {
    name: &'static str,
    slot: Arc<Mutex<Option<T>>>,
}

impl<T> Clone for Report<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            slot: self.slot.clone(),
        }
    }
}

impl<T> Report<T>
where
    T: for<'de> Deserialize<'de> + JsonSchema + Send + 'static,
{
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            slot: Arc::default(),
        }
    }

    /// The tool that fills the report; a second call replaces the first.
    pub fn tool(&self, description: &str) -> Tool {
        let slot = self.slot.clone();
        Tool::new(
            self.name,
            description,
            move |call: ToolCall<T>| async move {
                *slot.lock().unwrap() = Some(call.data);
                Ok::<_, Infallible>("Recorded.")
            },
        )
    }

    /// Asks `instance` `message` and takes its report, asking once more if it ended without one.
    pub async fn ask(&self, instance: &AgentInstance, message: String) -> anyhow::Result<T> {
        self.slot.lock().unwrap().take();
        instance.ask(message).await?;
        if let Some(report) = self.slot.lock().unwrap().take() {
            return Ok(report);
        }
        let nudge = format!("You ended without calling `{}`. Call it now.", self.name);
        instance.ask(nudge).await?;
        let report = self.slot.lock().unwrap().take();
        report.ok_or_else(|| {
            anyhow::anyhow!("`{}` ended without calling `{}`", instance.id(), self.name)
        })
    }
}
