//! The ids that name an agent instance and a dispatch.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The name of one agent instance: a later
/// [`Runtime::instance`](crate::Runtime::instance) with the same id continues the
/// same conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name keelrs gives one dispatch when it is made: unique everywhere and
/// kept across restarts. The receipt carries it, and every event the dispatch
/// causes is marked with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DispatchId(uuid::Uuid);

impl DispatchId {
    /// A new UUIDv7.
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl fmt::Display for DispatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
