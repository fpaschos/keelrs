//! keelrs — a Flue-style agent framework for Rust.
//!
//! The model reasons; keelrs gives it a model, a sandbox, tools and conversations.
//! Terms follow `GLOSSARY.md`.

pub mod agent;
pub mod model;
pub mod runtime;
pub mod sandbox;
pub mod skill;

mod driver;
mod error;
mod event;
mod tool;

pub use agent::{Agent, AgentContext};
pub use error::{Error, Outcome, Result};
pub use model::{Model, Thinking};
pub use runtime::{AgentInstance, Init, Receipt, Reply, Runtime, SubmissionId};
pub use skill::Skill;
