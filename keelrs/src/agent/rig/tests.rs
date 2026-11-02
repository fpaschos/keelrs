//! The tool loop, driven on rig-core's scripted model: no network; and one
//! call through [`call`], the seam the loop runs each call with.
//!
//! Unit tests, not `keelrs/tests/`: they inject the model through the private
//! `run_with` and read Rig's messages from `Settled`, neither of which the
//! public API exposes (Rig types stay in this module, ADR 0004).

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rig_core::completion::AssistantContent;
use rig_core::message::{ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Barrier;

use super::*;
use crate::sandbox::unused;
use crate::skills::tests::{Folder, haiku, sonnet};
use crate::{Agent, AgentContext, InstanceId, Skill, ToolCall};

/// Runs `agent` on one message against a model that replies with `turns`.
async fn scripted(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> Result<Settled, String> {
    scripted_in(agent, turns, &mut Caches::default()).await
}

/// [`scripted`], as the agent instance whose built-in tools remember in `caches`.
async fn scripted_in(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
    caches: &mut Caches,
) -> Result<Settled, String> {
    let model = MockCompletionModel::from_turns(turns);
    run_with(
        agent.bind(InstanceId::new("test"), ()),
        "Go".to_owned(),
        Vec::new(),
        caches,
        |_| Ok(model.clone().erase()),
    )
    .await
}

/// Every tool result the run accepted, in order, as (text or JSON, is_error).
fn results(settled: &Settled) -> Vec<(String, bool)> {
    let mut results = Vec::new();
    for message in &settled.messages {
        let RigMessage::User { content, .. } = message.0.as_ref() else {
            continue;
        };
        for content in content.iter() {
            if let UserContent::ToolResult(result) = content {
                let text = result
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ToolResultContent::Text(t) => Some(t.text.clone()),
                        ToolResultContent::Json { value } => Some(value.to_string()),
                        ToolResultContent::Image(_) => None,
                    })
                    .collect::<String>();
                results.push((text, result.is_error));
            }
        }
    }
    results
}

/// An agent on the mock model, in an unused sandbox with no built-ins.
fn agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    let sb = unused();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only([]);
        declare(cx);
    })
}

/// An agent on the mock model whose `declare` calls `use_sandbox` itself.
fn sandboxed_agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    Agent::new("scripted", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        declare(cx);
        String::new()
    })
}

fn clock() -> Tool {
    Tool::host(
        "current_time",
        "The current time.",
        async |_: ToolCall<()>| Ok::<_, Infallible>("noon"),
    )
}

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
    let root = crate::sandbox::local()
        .root(std::env::temp_dir())
        .open()
        .await
        .unwrap();
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

// One call, through the seam `answer` runs it with.

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

#[tokio::test]
async fn a_call_gets_its_arguments_and_id_and_settles_to_its_value() {
    let called = call(
        &lookup_order(),
        json!({"order_id": "A1"}),
        "c1".into(),
        &unused(),
    )
    .await;
    assert_eq!(called, Ok(json!({"order": "A1", "call": "c1"})));
}

#[tokio::test]
async fn an_app_error_fails_the_call_as_other_with_its_message() {
    let tool = Tool::host(
        "lookup_order",
        "Look up an order.",
        async |call: ToolCall<Order>| Err::<(), _>(format!("no order {}", call.data.order_id)),
    );
    let called = call(&tool, json!({"order_id": "Z9"}), "c1".into(), &unused()).await;
    assert_eq!(called, Err(Failed::new(FailKind::Other, "no order Z9")));
}

#[tokio::test]
async fn arguments_that_break_the_schema_are_invalid_args_and_the_tool_does_not_run() {
    let ran = Arc::new(AtomicBool::new(false));
    let tool = Tool::host("lookup_order", "Look up an order.", {
        let ran = ran.clone();
        async move |_: ToolCall<Order>| {
            ran.store(true, Ordering::SeqCst);
            Ok::<_, Infallible>("found")
        }
    });
    let failed = call(&tool, json!({"order_id": 5}), "c1".into(), &unused())
        .await
        .unwrap_err();
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(failed.kind, FailKind::InvalidArgs);
    assert!(failed.message.contains("do not fit its schema"), "{failed}");
}

#[derive(Deserialize, JsonSchema)]
struct Ping {
    /// The schema takes any string; only an IPv4 address decodes.
    #[schemars(with = "String")]
    host: std::net::Ipv4Addr,
}

#[tokio::test]
async fn arguments_the_schema_takes_but_that_do_not_decode_are_invalid_args() {
    let tool = Tool::host("ping", "Ping a host.", async |call: ToolCall<Ping>| {
        Ok::<_, Infallible>(call.data.host.to_string())
    });
    let failed = call(&tool, json!({"host": "nowhere"}), "c1".into(), &unused())
        .await
        .unwrap_err();
    assert_eq!(failed.kind, FailKind::InvalidArgs);
    assert!(
        failed
            .message
            .starts_with("`ping`'s arguments do not fit: "),
        "{failed}"
    );
}

#[tokio::test]
async fn a_call_past_its_timeout_fails_as_timed_out() {
    let tool = Tool::host("slow", "Never finishes.", async |_: ToolCall<()>| {
        std::future::pending::<Result<(), Infallible>>().await
    })
    .timeout(Duration::from_millis(10));
    let called = call(&tool, json!({}), "c1".into(), &unused()).await;
    assert_eq!(
        called,
        Err(Failed::new(
            FailKind::Timeout,
            "tool \"slow\" timed out after 10ms"
        ))
    );
}

#[test]
fn a_failed_call_reaches_rig_with_its_kind_and_message() {
    for (kind, rig) in [
        (FailKind::InvalidArgs, ToolErrorKind::InvalidArgs),
        (FailKind::NotFound, ToolErrorKind::NotFound),
        (FailKind::PermissionDenied, ToolErrorKind::PermissionDenied),
        (FailKind::Timeout, ToolErrorKind::Timeout),
        (FailKind::Other, ToolErrorKind::Other),
    ] {
        let answered = result(Err(Failed::new(kind, "why")));
        let error = answered.error().expect("a failure");
        assert_eq!((error.kind(), error.message()), (rig, "why"));
    }
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
    let both = MockTurn::from_contents([("c1", "second"), ("c2", "first")].map(|(id, name)| {
        let function = ToolFunction::new(ToolName::new(name).unwrap(), json!({}));
        AssistantContent::ToolCall(rig_core::message::ToolCall::from_wire(id, function))
    }));
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

// Skills: the catalog and the built-in `activate_skill` and `read_skill_resource`.

/// Runs `agent` like [`scripted`] and returns every request the model got.
async fn sent(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Settled, Vec<CompletionRequest>) {
    sent_as(agent, InstanceId::new("test"), (), turns).await
}

/// [`sent`], as instance `id` of `agent` with `data`.
async fn sent_as<D: Send + Sync + 'static>(
    agent: &Agent<D>,
    id: InstanceId,
    data: D,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Settled, Vec<CompletionRequest>) {
    let model = MockCompletionModel::from_turns(turns);
    let settled = run_with(
        agent.bind(id, data),
        "Go".to_owned(),
        Vec::new(),
        &mut Caches::default(),
        |_| Ok(model.clone().erase()),
    )
    .await
    .unwrap();
    (settled, model.requests())
}

/// The preamble a request carried.
fn preamble(request: &CompletionRequest) -> Option<&str> {
    match request.chat_history.first()?.as_ref() {
        RigMessage::System { content } => Some(content),
        _ => None,
    }
}

/// The names of the tools a request declared.
fn tool_names(request: &CompletionRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

#[tokio::test]
async fn the_catalog_follows_the_instructions_in_use_skill_order() {
    let sb = unused();
    let agent = Agent::new("poet", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        cx.use_sandbox(&sb).only([]);
        cx.use_skills([sonnet(), haiku()]);
        "Write poems.".into()
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(
        preamble(&requests[0]),
        Some(
            "Write poems.\n\n\
             ## Available Skills\n\n\
             When a task matches a skill description, call the `activate_skill` tool with \
             that skill name before proceeding so its full instructions are loaded.\n\n\
             - **sonnet** — Write a sonnet.\n\
             - **haiku** — Write a haiku."
        )
    );
}

#[tokio::test]
async fn activate_skill_is_declared_with_skills_and_read_skill_resource_with_files() {
    let without_files = agent(|cx| cx.use_skill(haiku()));
    let (_, requests) = sent(&without_files, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["activate_skill"]);

    let with_files = agent(|cx| {
        cx.use_tool(clock());
        cx.use_skills([haiku(), sonnet()]);
    });
    let (_, requests) = sent(&with_files, [MockTurn::text("Done.")]).await;
    assert_eq!(
        tool_names(&requests[0]),
        ["activate_skill", "read_skill_resource", "current_time"]
    );
}

#[tokio::test]
async fn a_turn_without_skills_or_a_sandbox_declares_only_its_own_tools() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(preamble(&requests[0]), None);
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

#[tokio::test]
async fn use_tools_declares_each_tool_in_order_by_reference() {
    let (clock, lookup) = (clock(), lookup_order());
    let agent = agent(move |cx| cx.use_tools([&lookup, &clock]));
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["lookup_order", "current_time"]);
}

#[tokio::test]
async fn a_render_reads_its_instances_data_and_id() {
    let sb = unused();
    let agent = Agent::new("issue", move |cx: &mut AgentContext<u64>| {
        cx.use_model("mock/model");
        cx.use_sandbox(&sb).only([]);
        format!("You implement #{} as {}.", cx.data(), cx.id())
    });
    let (_, requests) = sent_as(
        &agent,
        InstanceId::new("implement-17"),
        17,
        [MockTurn::text("Done.")],
    )
    .await;
    assert_eq!(
        preamble(&requests[0]),
        Some("You implement #17 as implement-17.")
    );
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

#[tokio::test]
async fn a_multi_line_description_is_one_catalog_line() {
    let agent = agent(|cx| {
        cx.use_skill(Skill::new("haiku", "Write a haiku.\nShort ones.\n", "Body.").unwrap());
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert!(
        preamble(&requests[0])
            .unwrap()
            .ends_with("\n\n- **haiku** — Write a haiku. Short ones."),
        "{:?}",
        preamble(&requests[0])
    );
}

// Built-in tools: they arrive with `use_sandbox`, first in the tool list.

/// A local sandbox on a fresh temp folder, which is removed on drop.
async fn sandboxed() -> (Folder, Sandbox) {
    let folder = Folder::new();
    let sb = crate::sandbox::local()
        .root(&folder.0)
        .open()
        .await
        .unwrap();
    (folder, sb)
}

#[tokio::test]
async fn use_sandbox_alone_declares_the_builtins_before_skill_tools_and_declared_tools() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_tool(clock());
        cx.use_skill(haiku());
        cx.use_sandbox(&sb);
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    let expected: Vec<&str> = builtin::NAMES
        .into_iter()
        .chain(["activate_skill", "current_time"])
        .collect();
    assert_eq!(tool_names(&requests[0]), expected);
}

#[tokio::test]
async fn only_keeps_the_named_builtins_in_their_own_order() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only(["bash", "glob", "grep", "read"]);
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["read", "bash", "grep", "glob"]);
}

#[tokio::test]
async fn only_with_no_names_keeps_only_the_declared_tools() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only([]);
        cx.use_tool(clock());
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

#[tokio::test]
async fn an_unknown_name_in_only_fails_the_run() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only(["read", "shell"]);
    });
    let failed = scripted(&agent, [MockTurn::text("Done.")])
        .await
        .unwrap_err();
    assert_eq!(
        failed,
        "`only` names `shell`, which is not a built-in tool; they are \
         read, write, edit, bash, grep, glob, delete, move"
    );
}

#[tokio::test]
async fn a_builtin_name_is_reserved_with_or_without_only() {
    let (_folder, sb) = sandboxed().await;
    for name in builtin::NAMES {
        let mine = Tool::host(name, "Mine.", async |_: ToolCall<()>| {
            Ok::<_, Infallible>("ran")
        });
        let all = sandboxed_agent({
            let (sb, mine) = (sb.clone(), mine.clone());
            move |cx| {
                cx.use_sandbox(&sb);
                cx.use_tool(mine.clone());
            }
        });
        let left_out = agent(move |cx| cx.use_tool(mine.clone()));
        for agent in [all, left_out] {
            let failed = scripted(&agent, [MockTurn::text("Done.")])
                .await
                .unwrap_err();
            assert_eq!(
                failed,
                format!("`{name}` is reserved for keelrs's built-in tool")
            );
        }
    }
}

/// An agent on the mock model whose built-ins act through `sb`.
fn agent_in(sb: &Sandbox) -> Agent {
    let sb = sb.clone();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb);
    })
}

/// The model reads `path`, then is done.
fn read(path: &str) -> [MockTurn; 2] {
    [
        MockTurn::tool_call("c1", "read", json!({"path": path})),
        MockTurn::text("Done."),
    ]
}

/// The model edits `one` to `1` in `path`, then is done.
fn edit_one(path: &str) -> [MockTurn; 2] {
    [
        MockTurn::tool_call(
            "c1",
            "edit",
            json!({"path": path, "edits": [{"old_text": "one", "new_text": "1"}]}),
        ),
        MockTurn::text("Done."),
    ]
}

const STALE: &str = "changed since it was last read";

#[tokio::test]
async fn each_instance_remembers_only_what_its_own_builtins_read() {
    let (folder, sb) = sandboxed().await;
    folder.write("a.txt", "one\n");
    let (mut reader, mut other) = (Caches::default(), Caches::default());
    scripted_in(&agent_in(&sb), read("a.txt"), &mut reader)
        .await
        .unwrap();
    folder.write("a.txt", "two\n");

    let theirs = results(
        &scripted_in(&agent_in(&sb), edit_one("a.txt"), &mut other)
            .await
            .unwrap(),
    );
    assert!(!theirs[0].0.contains(STALE), "{theirs:?}");
    let mine = results(
        &scripted_in(&agent_in(&sb), edit_one("a.txt"), &mut reader)
            .await
            .unwrap(),
    );
    assert!(mine[0].0.contains(STALE), "{mine:?}");
}

#[tokio::test]
async fn a_render_in_another_sandbox_forgets_what_the_builtins_read() {
    let (folder, first) = sandboxed().await;
    let second = crate::sandbox::local()
        .root(&folder.0)
        .open()
        .await
        .unwrap();
    folder.write("a.txt", "one\n");
    let mut caches = Caches::default();
    scripted_in(&agent_in(&first), read("a.txt"), &mut caches)
        .await
        .unwrap();
    folder.write("a.txt", "two\n");
    scripted_in(&agent_in(&second), [MockTurn::text("Done.")], &mut caches)
        .await
        .unwrap();

    // Back in the first sandbox, the edit reads the file afresh, not the old read.
    let edited = scripted_in(&agent_in(&first), edit_one("a.txt"), &mut caches)
        .await
        .unwrap();
    let edited = results(&edited);
    assert!(
        edited[0].0.starts_with("`old_text` was not found"),
        "{edited:?}"
    );
}

#[tokio::test]
async fn a_work_dir_of_the_same_sandbox_keeps_what_the_builtins_read() {
    let (folder, root) = sandboxed().await;
    folder.write("sub/a.txt", "one\n");
    let mut caches = Caches::default();
    scripted_in(&agent_in(&root), read("sub/a.txt"), &mut caches)
        .await
        .unwrap();
    folder.write("sub/a.txt", "two\n");

    let sub = root.work_dir("sub");
    let edited = scripted_in(&agent_in(&sub), edit_one("a.txt"), &mut caches)
        .await
        .unwrap();
    let edited = results(&edited);
    assert!(edited[0].0.contains(STALE), "{edited:?}");
}

#[tokio::test]
async fn a_failed_render_in_another_sandbox_keeps_what_the_builtins_read() {
    let (folder, first) = sandboxed().await;
    folder.write("a.txt", "one\n");
    let mut caches = Caches::default();
    scripted_in(&agent_in(&first), read("a.txt"), &mut caches)
        .await
        .unwrap();
    folder.write("a.txt", "two\n");
    let unknown = sandboxed_agent(|cx| {
        cx.use_sandbox(&unused()).only(["shell"]);
    });
    scripted_in(&unknown, [MockTurn::text("Done.")], &mut caches)
        .await
        .unwrap_err();

    let edited = scripted_in(&agent_in(&first), edit_one("a.txt"), &mut caches)
        .await
        .unwrap();
    let edited = results(&edited);
    assert!(edited[0].0.contains(STALE), "{edited:?}");
}
