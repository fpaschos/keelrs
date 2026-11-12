//! Errors returned by keelrs.

use crate::sandbox::SandboxError;
use crate::store::StoreError;
use crate::{DispatchId, InstanceId};

/// Something keelrs could not do.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The agent was never given to [`Runtime::start`](crate::Runtime::start).
    #[error("unknown agent `{0}`")]
    UnknownAgent(String),
    /// Two agents given to [`Runtime::start`](crate::Runtime::start) share a name.
    #[error("two agents are named `{0}`; agent names must be unique")]
    DuplicateAgent(String),
    /// An instance was asked for outside a Tokio runtime, which its actor needs.
    #[error("agent instances must be created within a Tokio runtime")]
    NoTokioRuntime,
    /// An instance was asked for by a blank id.
    #[error("instance id is blank")]
    InvalidInstanceId(InstanceId),
    /// An existing instance was asked for with data other than it was created with.
    #[error("instance `{id}` of agent `{agent}` exists with other data")]
    InstanceDataConflict {
        /// The agent's name.
        agent: String,
        /// The instance's id.
        id: InstanceId,
    },
    /// An instance's data could not be serialized, so it cannot be kept or compared.
    #[error("data for instance `{id}` of agent `{agent}` could not be serialized: {reason}")]
    InvalidInstanceData {
        /// The agent's name.
        agent: String,
        /// The instance's id.
        id: InstanceId,
        /// Why serialization failed, as plain text.
        reason: String,
    },
    /// A read named a dispatch this instance never received.
    #[error("no dispatch {0}")]
    DispatchNotFound(DispatchId),
    /// The dispatched message settled without a reply.
    #[error("agent run {} (dispatch {dispatch_id})", outcome.describe())]
    Run {
        /// How the message settled.
        outcome: Outcome,
        /// The dispatch that settled.
        dispatch_id: DispatchId,
    },
    /// A sandbox could not do what was asked.
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    /// The store could not do what was asked.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// How a dispatched message settled when it produced no reply.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The run ended in an error.
    Failed {
        /// What went wrong, as plain text (e.g. which environment variable to set).
        reason: String,
    },
    /// The run was stopped before it finished.
    Aborted,
}

impl Outcome {
    fn describe(&self) -> String {
        match self {
            Self::Failed { reason } => format!("failed: {reason}"),
            Self::Aborted => "was aborted".to_owned(),
        }
    }
}

/// `Result` with keelrs's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
