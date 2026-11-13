//! keelrs through its public API, one module per feature; agents run on a
//! scripted model ([`keelrs::testing::ScriptedModel`]), so nothing reaches the network.

mod builtin;
mod discover;
mod dispatch;
mod durable;
mod instance;
mod lifecycle;
mod locks;
mod sandbox;
mod sandbox_backend;
mod sandbox_exec;
mod sandbox_lock;
mod sandbox_run;
mod skills;
mod structured;
mod support;
mod turn_setup;
mod turns;
