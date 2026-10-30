//! `pwd`: a sandboxed tool, run in the sandbox its turn declares.

use keelrs::sandbox::{ExecRequest, Exit, Sandbox};
use keelrs::{Tool, ToolCall};

use crate::failure::{Failure, fail};

pub(crate) fn pwd() -> Tool {
    Tool::sandbox(
        "pwd",
        "The directory your commands run in.",
        async |_: ToolCall<()>, sandbox: Sandbox| {
            let ran = sandbox
                .exec(ExecRequest::argv(["pwd"]))
                .await
                .map_err(|e| Failure(e.to_string()))?
                .run()
                .await
                .map_err(|e| Failure(e.to_string()))?;
            if ran.exit != Exit::Code(0) {
                return fail(ran.stderr.trim().to_owned());
            }
            Ok(ran.stdout.trim().to_owned())
        },
    )
}
