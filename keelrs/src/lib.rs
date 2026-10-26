//! keelrs — a Flue-style agent harness for Rust.
//!
//! The LLM is the brain; the harness gives it tools, a sandbox, and sessions.
//! See `docs/flue-rust-harness-conversation.md` for the design notes.

pub mod agent;
pub mod event;
pub mod model;
pub mod sandbox;
pub mod session;
pub mod tool;
