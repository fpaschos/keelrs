//! keelrs — a Flue-style agent framework for Rust.
//!
//! The model reasons; keelrs gives it a model, a sandbox, tools and conversations.
//! Terms follow `GLOSSARY.md`.

pub mod agent;
pub mod model;
pub mod runtime;
pub mod sandbox;
pub mod skill;

mod error;
mod event;
mod tool;

pub use agent::{Agent, AgentContext, AgentInstance, Receipt, Reply, SubmissionId};
pub use error::{Error, Outcome, Result};
pub use model::{Model, Thinking};
pub use runtime::{Init, Runtime};
pub use skill::Skill;
pub use tool::{Tool, ToolCall};
