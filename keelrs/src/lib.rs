//! keelrs — a Flue-style agent framework for Rust.
//!
//! The model reasons; keelrs gives it a model, a sandbox, tools and conversations.
//! Terms follow `GLOSSARY.md`.

pub mod agent;
pub mod model;
pub mod runtime;
pub mod sandbox;
pub mod skills;
pub mod store;
#[cfg(feature = "testing")]
pub mod testing;

#[doc(hidden)]
pub mod __private {
    pub use include_dir;
}

mod builtin;
mod error;
mod event;
#[cfg(test)]
mod test_support;
mod tool;

pub use agent::{Agent, AgentContext, AgentInstance, DispatchId, InstanceId, Receipt, Reply};
/// Implements a [`SandboxBackend`](sandbox::SandboxBackend) with `async fn`s.
pub use async_trait::async_trait;
pub use error::{Error, Outcome, Result};
pub use model::{Model, Thinking};
pub use runtime::{Agents, AnyAgent, Runtime, RuntimeBuilder};
pub use skills::Skill;
pub use tool::{Tool, ToolCall};
