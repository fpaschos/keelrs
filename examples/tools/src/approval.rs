//! `record_approval`: sets the shared flag that makes `issue_refund` appear next turn.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct RecordApproval {
    /// The operator's approval code.
    code: String,
}

pub(crate) fn record_approval(approved: Arc<AtomicBool>) -> Tool {
    Tool::new(
        "record_approval",
        "Record an operator approval code; refunds become available once it is valid.",
        async move |call: ToolCall<RecordApproval>| {
            let valid = call.data.code == "7731";
            approved.store(valid, Ordering::SeqCst);
            Ok::<_, Infallible>(if valid {
                "Approval recorded."
            } else {
                "That code is not valid."
            })
        },
    )
}
