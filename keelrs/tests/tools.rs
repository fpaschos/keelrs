//! Declaring tools with `use_tool`, through the public API, with no network.

use std::convert::Infallible;

mod common;

use common::temp_sandbox;
use keelrs::sandbox::Sandbox;
use keelrs::{Agent, AgentContext, Error, InstanceId, Outcome, Runtime, Tool, ToolCall};

/// Asks a fresh instance of `agent` one message and returns its failure reason.
async fn failure_reason(agent: Agent) -> String {
    let runtime = Runtime::start([&agent]).unwrap();
    let instance = runtime.instance(&agent, InstanceId::new("test")).unwrap();
    match instance.ask("Say hello").await {
        Err(Error::Run {
            outcome: Outcome::Failed { reason },
            ..
        }) => reason,
        other => panic!("expected a failed run, got {other:?}"),
    }
}

fn clock() -> Tool {
    Tool::host(
        "current_time",
        "The current time.",
        async |_: ToolCall<()>| Ok::<_, Infallible>("noon"),
    )
}

#[tokio::test]
async fn two_tools_with_one_name_fail() {
    let sb = temp_sandbox().await;
    let agent = Agent::new("duplicate-tools", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_sandbox(&sb).only([]);
        cx.use_tool(clock());
        cx.use_tool(clock());
        "Tell the time.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(
        reason.contains("two tools are named `current_time`"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_tool_whose_arguments_are_not_an_object_fails() {
    let sb = temp_sandbox().await;
    let agent = Agent::new("string-args", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_sandbox(&sb).only([]);
        cx.use_tool(Tool::host(
            "echo",
            "Echo the text.",
            async |call: ToolCall<String>| Ok::<_, Infallible>(call.data),
        ));
        "Echo.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(
        reason.contains("`echo`'s arguments must be a JSON object"),
        "{reason}"
    );
}

fn listing() -> Tool {
    Tool::sandbox(
        "list_files",
        "The files in the work dir.",
        async |_: ToolCall<()>, sb: Sandbox| {
            sb.list(".", None, keelrs::sandbox::Listing::Visible).await
        },
    )
}

#[tokio::test]
async fn use_sandbox_twice_fails() {
    let sb = temp_sandbox().await;
    let agent = Agent::new("two-sandboxes", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_sandbox(&sb);
        cx.use_sandbox(&sb);
        "List files.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(
        reason.contains("`use_sandbox` was called 2 times in one turn setup; call it once"),
        "{reason}"
    );
}

#[tokio::test]
async fn an_agent_that_never_calls_use_sandbox_fails() {
    let agent = Agent::new("no-sandbox", |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_tool(listing());
        "List files.".into()
    });
    let reason = failure_reason(agent).await;
    assert_eq!(reason, "the agent function never called `use_sandbox`");
}

#[tokio::test]
async fn a_missing_model_fails_before_a_missing_sandbox() {
    let agent = Agent::new("nothing", |_: &mut AgentContext| "Hello.".into());
    let reason = failure_reason(agent).await;
    assert_eq!(reason, "the agent function never called `use_model`");
}
