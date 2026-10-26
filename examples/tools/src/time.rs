//! `current_time`: a tool with no arguments.

use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use keelrs::{Tool, ToolCall};

pub(crate) fn current_time() -> Tool {
    Tool::new(
        "current_time",
        "The current time, as seconds since the Unix epoch (UTC).",
        async |_: ToolCall<()>| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            Ok::<_, Infallible>(format!("{} seconds since the Unix epoch", now.as_secs()))
        },
    )
}
