//! Errors returned by keelrs.

use std::fmt;

use crate::SubmissionId;

/// Something keelrs could not do.
#[derive(Debug)]
pub enum Error {
    /// The agent was never given to [`Runtime::start`](crate::Runtime::start).
    UnknownAgent(String),
    /// A read addressed an instance that was never dispatched to.
    InstanceNotFound(String),
    /// A read named a submission the instance never received.
    SubmissionNotFound(SubmissionId),
    /// The dispatched message settled without a reply.
    Run {
        outcome: Outcome,
        submission_id: SubmissionId,
    },
}

/// How a dispatched message settled when it produced no reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Failed,
    Aborted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownAgent(name) => write!(f, "unknown agent `{name}`"),
            Self::InstanceNotFound(id) => write!(f, "no agent instance `{id}`"),
            Self::SubmissionNotFound(id) => write!(f, "no submission {id}"),
            Self::Run {
                outcome: Outcome::Failed,
                submission_id,
            } => {
                write!(f, "agent run failed (submission {submission_id})")
            }
            Self::Run {
                outcome: Outcome::Aborted,
                submission_id,
            } => {
                write!(f, "agent run was aborted (submission {submission_id})")
            }
        }
    }
}

impl std::error::Error for Error {}

/// `Result` with keelrs's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
