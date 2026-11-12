//! The Rig integration: steps one Rig `AgentRun` per dispatch; the only module where Rig types appear (ADR 0004).
//!
//! Each turn's tools go to the model and stay pinned for the `CallTools`
//! step that follows its model call (How a tool call runs).
//!
//! `history.rs` holds the messages a run accepted; `run.rs` the stepper;
//! `call.rs` how one tool call runs.

mod call;
mod history;
mod run;

pub(crate) use call::args_schema;
#[cfg(test)]
pub(crate) use call::call;
pub(crate) use history::{Message, ToolResultPart, close_pending, record};
pub(crate) use run::{Connect, Run, Step, connect_catalog};

#[cfg(test)]
pub(crate) mod tests;
