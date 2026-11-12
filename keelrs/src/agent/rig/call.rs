//! How one tool call runs: the schema refusal, decoding, the run and the
//! timeout, and what Rig is answered with.

use std::collections::HashMap;

use rig_agent::run::{PendingToolCall, ToolAnswer};
use rig_core::message::{CallId, ToolCall as RigToolCall, ToolFunction, ToolName};
use rig_core::tool::feedback::arguments_refusal;
use rig_core::tool::{ToolErrorKind, ToolExecutionError, ToolOutput, ToolResult};
use serde_json::Value;

use crate::Tool;
use crate::sandbox::Sandbox;
use crate::tool::{Called, FailKind, Failed};

/// Runs one call with its pinned tool and answers it, with the call's id; a model mistake is an
/// error result the model reads, never a failed run.
pub(super) async fn answer(
    call: PendingToolCall,
    tools: &HashMap<String, Tool>,
    sandbox: &Sandbox,
) -> Result<(CallId, ToolAnswer), String> {
    let exec = match call {
        PendingToolCall::Execute(call) => call,
        // Arguments that are not a JSON object: Rig's feedback asks for a retry.
        PendingToolCall::Malformed(call) => return Ok((call.id().clone(), call.answer(None))),
        other => return Err(format!("unsupported tool call {other:?}")),
    };
    // The run only emits calls the pinned turn setup declared.
    let tool = tools
        .get(&**exec.name())
        .ok_or_else(|| format!("tool \"{}\" was not declared this turn", exec.name()))?;
    let id = exec.id().wire().into_owned();
    let called = self::call(tool, exec.arguments(), id, sandbox).await;
    Ok((exec.id().clone(), exec.answer(result(called))))
}

/// What Rig is answered with for `called`: a string is text, `()` empty
/// text, any other value JSON, and a failure keeps its kind.
pub(super) fn result(called: Called) -> ToolResult {
    match called {
        Ok(Value::String(text)) => ToolResult::success(ToolOutput::text(text)),
        Ok(Value::Null) => ToolResult::success(ToolOutput::text("")),
        Ok(value) => ToolResult::success(ToolOutput::json(value)),
        Err(failed) => ToolResult::failed(ToolExecutionError::new(
            kind_of(failed.kind),
            failed.message,
        )),
    }
}

/// Runs one call of `tool` exactly as a turn does: the schema refusal,
/// decoding, the run, the timeout. What the model reads is `Ok` or `Err`.
pub(crate) async fn call(tool: &Tool, arguments: Value, id: String, sandbox: &Sandbox) -> Called {
    // Rebuilt from the name and arguments: Rig answers a call with unparsed
    // arguments as malformed, so one that reaches a tool never has them.
    let name =
        ToolName::new(&*tool.name).map_err(|e| Failed::new(FailKind::Other, e.to_string()))?;
    let rig_call = RigToolCall::from_wire(id.clone(), ToolFunction::new(name, arguments));
    if let Some(refusal) = arguments_refusal(&tool.schema, &rig_call) {
        return Err(Failed::new(FailKind::InvalidArgs, refusal));
    }
    // The object the refusal checked, as a turn's `ExecCall::arguments` gives it.
    let arguments = Value::Object(rig_call.function.arguments);
    let running = (tool.run)(arguments, id, sandbox.clone());
    match tool.timeout {
        Some(limit) => tokio::time::timeout(limit, running)
            .await
            .unwrap_or_else(|_| {
                let message = format!("tool \"{}\" timed out after {limit:?}", tool.name);
                Err(Failed::new(FailKind::Timeout, message))
            }),
        None => running.await,
    }
}

/// Rig's kind for a keelrs one.
fn kind_of(kind: FailKind) -> ToolErrorKind {
    match kind {
        FailKind::InvalidArgs => ToolErrorKind::InvalidArgs,
        FailKind::NotFound => ToolErrorKind::NotFound,
        FailKind::PermissionDenied => ToolErrorKind::PermissionDenied,
        FailKind::Timeout => ToolErrorKind::Timeout,
        FailKind::Other => ToolErrorKind::Other,
    }
}

/// The JSON schema the model is sent for a tool's `Args`; `()` takes an
/// empty object.
pub(crate) fn args_schema<Args: schemars::JsonSchema>() -> serde_json::Value {
    let schema = rig_core::tool::args_schema::<Args>();
    if schema.get("type").and_then(|t| t.as_str()) == Some("null") {
        return serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false});
    }
    schema
}
