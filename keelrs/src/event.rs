//! keelrs's canonical events: the source of truth for an agent instance (ADR 0004).
//!
//! Private and minimal for now: just enough to rebuild the history for the next
//! dispatch and to answer reads, once durable storage keeps them (`STORE:`).

use crate::agent::rig::Message;
use crate::{Outcome, SubmissionId};

#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "STORE: read back once durable storage keeps events"
)]
pub(crate) enum Event {
    /// A message the run accepted (the user's prompt or the assistant's turn).
    Message(Message),
    /// The dispatched message settled: its reply text, or how it failed.
    Settled {
        submission_id: SubmissionId,
        result: Result<String, Outcome>,
    },
}
