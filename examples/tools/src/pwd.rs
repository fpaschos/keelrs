//! `pwd`: a sandboxed tool, run in the sandbox its turn declares.

use keelrs::sandbox::{ExecRequest, Exit, Sandbox};
use keelrs::{Tool, ToolCall};

pub(crate) fn pwd() -> Tool {
    Tool::sandbox(
        "pwd",
        "The directory your commands run in.",
        async |_: ToolCall<()>, sandbox: Sandbox| {
            let ran = sandbox
                .exec(ExecRequest::argv(["pwd"]))
                .await?
                .run()
                .await?;
            if ran.exit != Exit::Code(0) {
                anyhow::bail!("{}", ran.stderr.trim());
            }
            Ok(ran.stdout.trim().to_owned())
        },
    )
}
