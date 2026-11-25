//! The ids that name an agent instance and a dispatch.

use derive_more::Display;
use serde::{Deserialize, Serialize};

/// The name of one agent instance: a later
/// [`Runtime::instance`](crate::Runtime::instance) with the same id continues the
/// same conversation.
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// An id the caller chooses, such as `implement-17`.
    ///
    /// A blank id is accepted here and fails at
    /// [`Runtime::instance`](crate::Runtime::instance).
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// A new UUIDv7: unique across restarts, ordered by time.
    #[must_use]
    pub fn fresh() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The name keelrs gives one dispatch when it is made: unique everywhere and
/// kept across restarts. The receipt carries it, and every event the dispatch
/// causes is marked with it.
#[derive(Debug, Display, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DispatchId(uuid::Uuid);

impl DispatchId {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}
