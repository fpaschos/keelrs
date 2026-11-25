//! The Rig tests no public seam reaches; the rest go through `Runtime` in
//! `keelrs/tests/it/`. Shared helpers here, the tests by topic in the child
//! modules.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rig_core::tool::ToolErrorKind;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::call::result;
use super::*;
use crate::agent::TurnSetup;
use crate::builtin::Caches;
use crate::builtin::structured::Structured;
use crate::event::Event;
use crate::sandbox::unused;
use crate::tool::{FailKind, Failed};
use crate::{Agent, AgentContext, InstanceId, Tool, ToolCall};

mod call;
mod connect;
mod structured;

/// A run on "Go" of an agent on `mock/model` with no built-ins and `tools`,
/// its model connected by `connect`.
fn mock_run(tools: Vec<Tool>, structured: Option<Structured>, connect: Connect) -> Run {
    let agent = Agent::new("scripted", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        cx.use_sandbox(&unused()).only([]);
        tools.iter().for_each(|tool| cx.use_tool(tool.clone()));
        String::new()
    });
    let setup = TurnSetup::new(&agent, InstanceId::new("test"), ());
    Run::new(
        setup,
        "Go".to_owned(),
        Vec::new(),
        structured,
        Caches::default(),
        connect,
    )
}

/// A sink that keeps what the run persisted.
impl PersistSink for &mut Vec<Event> {
    async fn persist(&mut self, events: Vec<Event>) {
        self.extend(events);
    }
}

#[derive(Deserialize, JsonSchema)]
struct Order {
    order_id: String,
}

fn lookup_order() -> Tool {
    Tool::host(
        "lookup_order",
        "Look up an order.",
        async |call: ToolCall<Order>| {
            Ok::<_, Infallible>(json!({"order": call.data.order_id, "call": call.id}))
        },
    )
}
