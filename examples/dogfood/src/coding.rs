//! rig-tools' coding tools (`read`, `write`, `edit`, `search`, `list`, `shell`) as keelrs tools,
//! on this machine from the process's working directory, until keelrs has its own (#26).
//!
//! keelrs runs a reply's calls side by side and does not read Rig's footprints, so two calls
//! that change one file can each write over the other; the implementer's instructions ask it
//! never to send such calls in one reply.

use std::sync::LazyLock;
use std::time::Duration;

use keelrs::{Tool, ToolCall};
use rig_core::tool::{Tool as RigTool, ToolContext};
use rig_tools::{Edit, List, Read, Search, Shell, Write};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

/// Longest a `shell` command runs; the model then reads that it timed out.
const SHELL_TIMEOUT: Duration = Duration::from_mins(5);

/// `read`, `search`, `list` and `shell`: looking around, and `git`/`gh` through `shell`.
pub static LOOKING: LazyLock<Vec<Tool>> = LazyLock::new(|| looking(Read::default()));

/// [`LOOKING`], plus `write` and `edit`, which share what its `read` has seen.
pub static CODING: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    let read = Read::default();
    let seen = read.seen.clone();
    let mut tools = looking(read);
    tools.push(rig(Write {
        seen: seen.clone(),
        ..Write::default()
    }));
    tools.push(rig(Edit {
        seen,
        ..Edit::default()
    }));
    tools
});

fn looking(read: Read) -> Vec<Tool> {
    vec![
        rig(read),
        rig(Search::default()),
        rig(List::default()),
        rig(Shell::default()).timeout(SHELL_TIMEOUT),
    ]
}

/// A Rig tool as a keelrs tool; each call is logged to stderr.
fn rig<T>(tool: T) -> Tool
where
    T: RigTool + Clone + 'static,
    T::Args: DeserializeOwned + JsonSchema + std::fmt::Debug + Send + 'static,
    T::Output: serde::Serialize + 'static,
    T::Error: Send + Sync,
{
    let description = tool.description();
    Tool::new(
        T::NAME,
        description,
        move |call: ToolCall<T::Args>| async move {
            eprintln!(
                "  {} {}",
                T::NAME,
                rig_tools::shorten(&format!("{:?}", call.data), 160)
            );
            tool.call(&mut ToolContext::new(), call.data).await
        },
    )
}
