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
use crate::Tool;
use crate::ToolCall;
use crate::sandbox::unused;
use crate::tool::{FailKind, Failed};

mod call;
mod structured;

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
