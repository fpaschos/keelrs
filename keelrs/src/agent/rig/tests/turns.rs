//! The tool loop: results reaching the model, concurrency, the turn cap, tool order.

use super::*;

#[tokio::test]
async fn a_tool_result_reaches_the_model_and_the_run_settles() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let settled = scripted(
        &agent,
        [
            MockTurn::tool_call("c1", "current_time", json!({})),
            MockTurn::text("It is noon."),
        ],
    )
    .await
    .unwrap();
    assert_eq!(settled.reply, "It is noon.");
    assert_eq!(results(&settled), [("noon".to_owned(), false)]);
}

#[tokio::test]
async fn a_sandboxed_tool_gets_the_sandbox_its_turn_declared() {
    let (_folder, root) = sandboxed().await;
    let declared = root.work_dir("keelrs-declared");
    let expected = json!({"id": declared.id().to_string(), "dir": declared.dir().to_string()});
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&declared);
        cx.use_tool(Tool::sandbox(
            "where_am_i",
            "Where the tool runs.",
            async |_: ToolCall<()>, sb: Sandbox| {
                Ok::<_, Infallible>(json!({"id": sb.id().to_string(), "dir": sb.dir().to_string()}))
            },
        ));
    });
    let results = one_call(&agent, "where_am_i", json!({})).await;
    assert_eq!(results, [(expected.to_string(), false)]);
}

/// One tool call, then the reply "Done.", and the results the run accepted.
async fn one_call(agent: &Agent, name: &str, arguments: Value) -> Vec<(String, bool)> {
    let settled = scripted(
        agent,
        [
            MockTurn::tool_call("c1", name, arguments),
            MockTurn::text("Done."),
        ],
    )
    .await
    .unwrap();
    results(&settled)
}

#[tokio::test]
async fn a_call_to_an_undeclared_tool_names_the_available_ones() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let results = one_call(&agent, "issue_refund", json!({})).await;
    assert_eq!(
        results,
        [(
            "tool \"issue_refund\" is not available; available tools: current_time".to_owned(),
            true
        )]
    );
}

#[tokio::test]
async fn one_replys_calls_run_concurrently_and_are_committed_in_call_order() {
    // Each call waits for the other: run one after another, they never finish.
    let barrier = Arc::new(Barrier::new(2));
    let agent = agent(move |cx| {
        for name in ["first", "second"] {
            let barrier = barrier.clone();
            cx.use_tool(Tool::host(
                name,
                "Waits for the other.",
                async move |_: ToolCall<()>| {
                    barrier.wait().await;
                    Ok::<_, Infallible>(name)
                },
            ));
        }
    });
    let both = reply([("c1", "second", json!({})), ("c2", "first", json!({}))]);
    let settled = tokio::time::timeout(
        Duration::from_secs(5),
        scripted(&agent, [both, MockTurn::text("Done.")]),
    )
    .await
    .expect("the calls ran one after another")
    .unwrap();
    assert_eq!(
        results(&settled),
        [("second".to_owned(), false), ("first".to_owned(), false)]
    );
}

#[tokio::test]
async fn reaching_the_turn_cap_fails_the_run() {
    let agent = agent(|cx| cx.use_tool(clock())).max_turns(2);
    let reason = scripted(
        &agent,
        [
            MockTurn::tool_call("c1", "current_time", json!({})),
            MockTurn::tool_call("c2", "current_time", json!({})),
            MockTurn::text("Never reached."),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(reason, "turn limit (2) reached");
}

#[tokio::test]
async fn use_tools_declares_each_tool_in_order_by_reference() {
    let (clock, lookup) = (clock(), lookup_order());
    let agent = agent(move |cx| cx.use_tools([&lookup, &clock]));
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["lookup_order", "current_time"]);
}

#[tokio::test]
async fn a_tool_may_take_a_name_keelrs_has_no_builtin_for() {
    for name in ["task", "finish", "give_up"] {
        let tool = Tool::host(name, "Mine.", async |_: ToolCall<()>| {
            Ok::<_, Infallible>("ran")
        });
        let agent = agent(move |cx| cx.use_tool(tool.clone()));
        let settled = scripted(&agent, [MockTurn::text("Done.")]).await;
        assert!(settled.is_ok(), "{name}: {settled:?}");
    }
}
