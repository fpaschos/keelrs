//! Errors returned by keelrs.

use crate::SubmissionId;

/// Something keelrs could not do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The agent was never given to [`Runtime::start`](crate::Runtime::start).
    #[error("unknown agent `{0}`")]
    UnknownAgent(String),
    /// A read named a submission this instance never received.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
