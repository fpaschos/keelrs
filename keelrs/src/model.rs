//! The model an agent declares with [`AgentContext::use_model`](crate::AgentContext::use_model).
//!
//! Plain data: the `provider/model` id and an optional thinking level. keelrs
//! connects it through Rig inside a private driver, so no Rig type appears here.

/// A model to call, as `provider/model` (e.g. `anthropic/claude-sonnet-5-5`).
///
/// The id passes unchanged to Rig's built-in catalog; its key comes from the
/// environment (e.g. `ANTHROPIC_API_KEY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// `provider/model`, passed unchanged to Rig.
    pub id: String,
    /// How much the model reasons; `None` sends nothing, so the model's own default applies.
    pub thinking: Option<Thinking>,
}

impl Model {
    /// The model `id`, as `provider/model`, with no thinking level set.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            thinking: None,
        }
    }

    /// Sets the thinking level.
    #[must_use]
    pub fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = Some(thinking);
        self
    }
}

impl From<&str> for Model {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for Model {
    fn from(id: String) -> Self {
        Self::new(id)
    }
}

/// How much a model reasons before it answers, as Flue's `thinkingLevel`.
///
/// Rig refuses a level the model does not list; the dispatch then settles as
/// [`Outcome::Failed`](crate::Outcome::Failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Thinking {
    /// No reasoning.
    Off,
    /// The least reasoning the model offers.
    Minimal,
    /// Low effort.
    Low,
    /// Medium effort.
    Medium,
    /// High effort.
    High,
    /// Extra-high effort.
    XHigh,
    /// The most reasoning the model offers.
    Max,
}
