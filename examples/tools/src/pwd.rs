//! `pwd`: a sandboxed tool, run in the sandbox its turn declares.

use keelrs::sandbox::Sandbox;
use keelrs::{Tool, ToolCall};

pub(crate) fn pwd() -> Tool {
    Tool::sandbox(
        "pwd",
        "The directory your commands run in.",
        async |_: ToolCall<()>, sandbox: Sandbox| sandbox.run(["pwd"]).await,
    )
}
