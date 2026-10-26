//! Errors returned by keelrs.

use crate::SubmissionId;

/// Something keelrs could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The agent was never given to [`Runtime::start`](crate::Runtime::start).
    #[error("unknown agent `{0}`")]
    UnknownAgent(String),
    /// A read addressed an instance that was never dispatched to.
    #[error("no agent instance `{0}`")]
    InstanceNotFound(String),
    /// A read named a submission the instance never received.
    #[error("no submission {0}")]
    SubmissionNotFound(SubmissionId),
    /// The dispatched message settled without a reply.
    #[error("agent run {} (submission {submission_id})", outcome.describe())]
    Run {
        /// How the message settled.
        outcome: Outcome,
        /// The message that settled.
        submission_id: SubmissionId,
    },
}

/// How a dispatched message settled when it produced no reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The run ended in an error.
    Failed,
    /// The run was stopped before it finished.
    Aborted,
}

impl Outcome {
    fn describe(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Aborted => "was aborted",
        }
    }
}

/// `Result` with keelrs's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
