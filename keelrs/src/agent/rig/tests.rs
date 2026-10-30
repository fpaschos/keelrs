//! The tool loop, driven on rig-core's scripted model: no network.
//!
//! Unit tests, not `keelrs/tests/`: they inject the model through the private
//! `run_with` and read Rig's messages from `Settled`, neither of which the
//! public API exposes (Rig types stay in this module, ADR 0004).

#![allow(clippy::disallowed_methods)] // skill folders are set up on the host

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use std::{fs, io};

use rig_core::completion::AssistantContent;
use rig_core::message::{ToolFunction, ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Barrier;

use super::*;
use crate::sandbox::Sandbox;
use crate::skills::{Discover, discover};
use crate::{AgentContext, Skill, Tool, ToolCall};

/// Runs `agent` on one message against a model that replies with `turns`.
async fn scripted(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> Result<Settled, String> {
    scripted_in(agent, turns, &Caches::default()).await
}

/// [`scripted`], as the agent instance whose built-in tools remember in `caches`.
async fn scripted_in(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
    caches: &Caches,
) -> Result<Settled, String> {
    let model = MockCompletionModel::from_turns(turns);
    run_with(agent.clone(), "Go".to_owned(), Vec::new(), caches, |_| {
        Ok(model.clone().erase())
    })
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

fn agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
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
    let agent = agent(move |cx| {
        cx.use_sandbox(declared.clone());
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

#[derive(Deserialize, JsonSchema)]
struct Order {
    order_id: String,
}

#[tokio::test]
async fn a_value_reaches_the_model_as_json_and_a_call_gets_its_id() {
    let agent = agent(|cx| {
        cx.use_tool(Tool::host(
            "lookup_order",
            "Look up an order.",
            async |call: ToolCall<Order>| {
                Ok::<_, Infallible>(json!({"order": call.data.order_id, "call": call.id}))
            },
        ));
    });
    let results = one_call(&agent, "lookup_order", json!({"order_id": "A1"})).await;
    assert_eq!(
        results,
        [(r#"{"order":"A1","call":"c1"}"#.to_owned(), false)]
    );
}

#[tokio::test]
async fn an_err_reaches_the_model_as_its_message() {
    let agent = agent(|cx| {
        cx.use_tool(Tool::host(
            "lookup_order",
            "Look up an order.",
            async |call: ToolCall<Order>| {
                Err::<(), _>(io::Error::other(format!("no order {}", call.data.order_id)))
            },
        ));
    });
    let results = one_call(&agent, "lookup_order", json!({"order_id": "Z9"})).await;
    assert_eq!(results, [("no order Z9".to_owned(), true)]);
}

#[tokio::test]
async fn arguments_that_break_the_schema_are_refused_and_the_tool_does_not_run() {
    let ran = Arc::new(AtomicBool::new(false));
    let agent = agent({
        let ran = ran.clone();
        move |cx| {
            let ran = ran.clone();
            cx.use_tool(Tool::host(
                "lookup_order",
                "Look up an order.",
                async move |_: ToolCall<Order>| {
                    ran.store(true, Ordering::SeqCst);
                    Ok::<_, Infallible>("found")
                },
            ));
        }
    });
    let results = one_call(&agent, "lookup_order", json!({"order_id": 5})).await;
    assert!(!ran.load(Ordering::SeqCst));
    let [(text, true)] = results.as_slice() else {
        panic!("expected one error result, got {results:?}");
    };
    assert!(text.contains("do not fit its schema"), "{text}");
}

#[tokio::test]
async fn a_call_past_its_timeout_reaches_the_model_as_timed_out() {
    let agent = agent(|cx| {
        cx.use_tool(
            Tool::host("slow", "Never finishes.", async |_: ToolCall<()>| {
                std::future::pending::<Result<(), Infallible>>().await
            })
            .timeout(Duration::from_millis(10)),
        );
    });
    let results = one_call(&agent, "slow", json!({})).await;
    assert_eq!(
        results,
        [("tool \"slow\" timed out after 10ms".to_owned(), true)]
    );
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
    let model = MockCompletionModel::from_turns(turns);
    let settled = run_with(
        agent.clone(),
        "Go".to_owned(),
        Vec::new(),
        &Caches::default(),
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

fn haiku() -> Skill {
    Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap()
}

fn sonnet() -> Skill {
    Skill::new("sonnet", "Write a sonnet.", "Fourteen lines.")
        .unwrap()
        .file("references/forms.md", "Petrarchan or Shakespearean.")
        .unwrap()
}

#[tokio::test]
async fn the_catalog_follows_the_instructions_in_use_skill_order() {
    let agent = Agent::new("poet", |cx: &mut AgentContext| {
        cx.use_model("mock/model");
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
async fn activating_a_skill_returns_its_instructions_and_lists_its_resources() {
    let agent = agent(|cx| cx.use_skills([haiku(), sonnet()]));
    let results = one_call(&agent, "activate_skill", json!({"name": "sonnet"})).await;
    assert_eq!(
        results,
        [(
            "Run the skill named \"sonnet\".\n\n\
             <skill_instructions>\nFourteen lines.\n</skill_instructions>\n\n\
             Supporting skill resources are available but are not loaded into context unless needed:\n\
             <skill_resources>\n- references/forms.md\n</skill_resources>"
                .to_owned(),
            false
        )]
    );
}

#[tokio::test]
async fn activating_a_skill_without_files_lists_no_resources() {
    let agent = agent(|cx| cx.use_skill(haiku()));
    let results = one_call(&agent, "activate_skill", json!({"name": "haiku"})).await;
    assert_eq!(
        results,
        [(
            "Run the skill named \"haiku\".\n\n\
             <skill_instructions>\nThree lines: 5, 7, 5.\n</skill_instructions>"
                .to_owned(),
            false
        )]
    );
}

#[tokio::test]
async fn activating_an_unknown_skill_names_the_available_ones() {
    let agent = agent(|cx| cx.use_skills([haiku(), sonnet()]));
    let results = one_call(&agent, "activate_skill", json!({"name": "limerick"})).await;
    assert_eq!(
        results,
        [(
            "skill \"limerick\" is not available; available skills: haiku, sonnet".to_owned(),
            false
        )]
    );
}

#[tokio::test]
async fn no_skills_no_catalog_and_no_skill_tools() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(preamble(&requests[0]), None);
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

/// A skill with one file at `references/file` holding `bytes`.
fn with_file(bytes: impl Into<Vec<u8>>) -> Skill {
    haiku().file("references/file", bytes).unwrap()
}

/// What `read_skill_resource` returns for `arguments`, with `skill` mounted.
async fn read(skill: Skill, arguments: Value) -> String {
    let agent = agent(move |cx| cx.use_skill(skill.clone()));
    let results = one_call(&agent, "read_skill_resource", arguments).await;
    let [(text, false)] = results.as_slice() else {
        panic!("expected one ordinary result, got {results:?}");
    };
    text.clone()
}

#[tokio::test]
async fn a_short_resource_is_read_whole() {
    let text = read(
        sonnet(),
        json!({"skill": "sonnet", "path": "references/forms.md"}),
    )
    .await;
    assert_eq!(text, "Petrarchan or Shakespearean.");
}

static REVIEW: Skill = crate::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/review");

#[tokio::test]
async fn an_embedded_skill_lists_and_reads_the_files_in_its_folder() {
    let agent = agent(|cx| cx.use_skill(&REVIEW));
    let results = one_call(&agent, "activate_skill", json!({"name": "review"})).await;
    assert!(
        results[0]
            .0
            .ends_with("<skill_resources>\n- references/style.md\n</skill_resources>"),
        "{results:?}"
    );
    let text = read(
        REVIEW.clone(),
        json!({"skill": "review", "path": "references/style.md"}),
    )
    .await;
    assert_eq!(text, "# Style\n\nShort sentences.");
}

#[tokio::test]
async fn a_resource_is_paged_by_offset_and_limit() {
    let skill = with_file("one\ntwo\nthree\nfour\nfive\n");
    let arguments = json!({"skill": "haiku", "path": "references/file", "offset": 2, "limit": 2});
    assert_eq!(
        read(skill, arguments).await,
        "two\nthree\n\n[Showing lines 2-3 of 5. Use offset=4 to continue.]"
    );
}

#[tokio::test]
async fn a_long_resource_stops_at_2000_lines() {
    let lines: Vec<String> = (1..=2001).map(|n| n.to_string()).collect();
    let skill = with_file(lines.join("\n"));
    let text = read(skill, json!({"skill": "haiku", "path": "references/file"})).await;
    assert!(text.starts_with("1\n2\n"), "{text}");
    assert!(
        text.ends_with("2000\n\n[Showing lines 1-2000 of 2001. Use offset=2001 to continue.]"),
        "{text}"
    );
}

#[tokio::test]
async fn a_wide_resource_stops_at_50_kb() {
    // 100 lines of 1023 characters plus a newline: 50 fit in 51 200 bytes.
    let line = "x".repeat(1023);
    let skill = with_file(vec![line.as_str(); 100].join("\n"));
    let text = read(skill, json!({"skill": "haiku", "path": "references/file"})).await;
    assert!(
        text.ends_with("\n\n[Showing lines 1-50 of 100. Use offset=51 to continue.]"),
        "{}",
        &text[text.len() - 80..]
    );
}

#[tokio::test]
async fn a_binary_resource_is_base64_wrapped_at_76_columns() {
    let text = read(
        with_file([0xff; 60]),
        json!({"skill": "haiku", "path": "references/file"}),
    )
    .await;
    assert_eq!(text, format!("{}\n////", "/".repeat(76)));
}

#[tokio::test]
async fn a_path_outside_the_skill_or_not_in_it_is_refused() {
    for (path, refusal) in [
        (
            "/references/forms.md",
            "must be relative to the skill's folder",
        ),
        (
            "../haiku/SKILL.md",
            "must be relative to the skill's folder",
        ),
        (
            "references/missing.md",
            "is not a file of skill \"sonnet\"; its files: references/forms.md",
        ),
    ] {
        let text = read(sonnet(), json!({"skill": "sonnet", "path": path})).await;
        assert!(text.contains(refusal), "{path}: {text}");
    }
    let text = read(sonnet(), json!({"skill": "limerick", "path": "x"})).await;
    assert_eq!(
        text,
        "skill \"limerick\" is not available; available skills: sonnet"
    );
}

// Discovered skills: files listed at discovery, read from disk on request.

/// A temp folder of skill folders, removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-rig-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// Writes `contents` at `relative`, creating its folders.
    fn write(&self, relative: &str, contents: &str) -> &Self {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    /// The one skill discovered in this folder.
    async fn skill(&self) -> Skill {
        let discovered = discover(Discover::new().root(&self.0)).await;
        let [skill] = <[Skill; 1]>::try_from(discovered.skills).unwrap();
        skill
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A folder holding the skill `lint` with two supporting files.
fn lint() -> Folder {
    let folder = Folder::new();
    folder
        .write(
            "lint/SKILL.md",
            "---\nname: lint\ndescription: Lint code.\n---\nRun the linter.\n",
        )
        .write("lint/references/rules.md", "No unused imports.")
        .write("lint/scripts/run.sh", "cargo clippy");
    folder
}

#[tokio::test]
async fn a_discovered_skill_lists_the_files_in_its_folder() {
    let folder = lint();
    let skill = folder.skill().await;
    let agent = agent(move |cx| cx.use_skill(skill.clone()));
    let results = one_call(&agent, "activate_skill", json!({"name": "lint"})).await;
    assert!(
        results[0].0.ends_with(
            "<skill_resources>\n- references/rules.md\n- scripts/run.sh\n</skill_resources>"
        ),
        "{results:?}"
    );
}

#[tokio::test]
async fn a_discovered_skill_reads_a_file_from_disk_on_request() {
    let folder = lint();
    let skill = folder.skill().await;
    folder.write("lint/references/rules.md", "No unused imports. Edited.");
    let text = read(
        skill,
        json!({"skill": "lint", "path": "references/rules.md"}),
    )
    .await;
    assert_eq!(text, "No unused imports. Edited.");
}

#[cfg(unix)]
#[tokio::test]
async fn a_discovered_skill_refuses_a_symlink_that_leaves_its_folder() {
    let folder = lint();
    let skill = folder.skill().await;
    folder.write("outside.md", "Not the skill's.");
    let rules = folder.0.join("lint/references/rules.md");
    fs::remove_file(&rules).unwrap();
    std::os::unix::fs::symlink(folder.0.join("outside.md"), &rules).unwrap();
    let text = read(
        skill,
        json!({"skill": "lint", "path": "references/rules.md"}),
    )
    .await;
    assert_eq!(text, "`references/rules.md` leaves the skill's folder");
}

#[tokio::test]
async fn a_discovered_skill_never_reads_a_secret() {
    let folder = lint();
    folder.write("lint/.env", "TOKEN=1");
    let skill = folder.skill().await;
    let text = read(skill, json!({"skill": "lint", "path": ".env"})).await;
    assert_eq!(
        text,
        "`.env` is not a file of skill \"lint\"; its files: references/rules.md, scripts/run.sh"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_discovered_skill_lists_a_symlink_that_stays_in_its_folder() {
    let folder = lint();
    std::os::unix::fs::symlink(folder.0.join("lint/references"), folder.0.join("lint/docs"))
        .unwrap();
    let skill = folder.skill().await;
    let agent = agent(move |cx| cx.use_skill(skill.clone()));
    let results = one_call(&agent, "activate_skill", json!({"name": "lint"})).await;
    assert!(
        results[0].0.ends_with(
            "<skill_resources>\n- docs/rules.md\n- references/rules.md\n- scripts/run.sh\n</skill_resources>"
        ),
        "{results:?}"
    );
}

#[tokio::test]
async fn a_tool_may_take_a_name_keelrs_has_no_builtin_for() {
    for name in ["task", "finish", "give_up"] {
        let tool = Tool::host(name, "Mine.", async |_: ToolCall<()>| {
            Ok::<_, Infallible>("ran")
        });
        let agent = agent(move |cx| cx.use_tool(tool.clone()));
        let results = one_call(&agent, name, json!({})).await;
        assert_eq!(results, [("ran".to_owned(), false)], "{name}");
    }
}

#[tokio::test]
async fn a_single_line_wider_than_50_kb_is_cut() {
    let skill = with_file("x".repeat(60 * 1024));
    let text = read(skill, json!({"skill": "haiku", "path": "references/file"})).await;
    assert_eq!(
        text,
        format!(
            "{}\n\n[Line 1 is cut at 51200 of 61440 bytes.]",
            "x".repeat(50 * 1024)
        )
    );
}

#[tokio::test]
async fn a_file_path_with_dot_segments_is_stored_and_read_normalized() {
    let skill = haiku().file("./refs//a/./b.md", "B").unwrap();
    let agent = agent({
        let skill = skill.clone();
        move |cx| cx.use_skill(skill.clone())
    });
    let results = one_call(&agent, "activate_skill", json!({"name": "haiku"})).await;
    assert!(
        results[0]
            .0
            .ends_with("<skill_resources>\n- refs/a/b.md\n</skill_resources>"),
        "{results:?}"
    );
    for path in ["refs/a/b.md", "./refs/a/b.md", "refs//a/./b.md"] {
        let text = read(skill.clone(), json!({"skill": "haiku", "path": path})).await;
        assert_eq!(text, "B", "{path}");
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
    let agent = agent(move |cx| {
        cx.use_tool(clock());
        cx.use_skill(haiku());
        cx.use_sandbox(sb.clone());
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    let expected: Vec<&str> = builtin::NAMES
        .into_iter()
        .chain(["activate_skill", "current_time"])
        .collect();
    assert_eq!(tool_names(&requests[0]), expected);
}

#[tokio::test]
async fn a_turn_without_a_sandbox_declares_no_builtins() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

#[tokio::test]
async fn only_keeps_the_named_builtins_in_their_own_order() {
    let (_folder, sb) = sandboxed().await;
    let agent = agent(move |cx| {
        cx.use_sandbox(sb.clone())
            .only(["bash", "glob", "grep", "read"]);
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["read", "bash", "grep", "glob"]);
}

#[tokio::test]
async fn only_with_no_names_keeps_only_the_declared_tools() {
    let (_folder, sb) = sandboxed().await;
    let agent = agent(move |cx| {
        cx.use_sandbox(sb.clone()).only([]);
        cx.use_tool(clock());
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

#[tokio::test]
async fn an_unknown_name_in_only_fails_the_run() {
    let (_folder, sb) = sandboxed().await;
    let agent = agent(move |cx| {
        cx.use_sandbox(sb.clone()).only(["read", "shell"]);
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
async fn a_builtin_name_is_reserved_with_or_without_a_sandbox_or_only() {
    let (_folder, sb) = sandboxed().await;
    for name in builtin::NAMES {
        let mine = Tool::host(name, "Mine.", async |_: ToolCall<()>| {
            Ok::<_, Infallible>("ran")
        });
        let sb = sb.clone();
        let without = agent({
            let mine = mine.clone();
            move |cx| cx.use_tool(mine.clone())
        });
        let left_out = agent(move |cx| {
            cx.use_sandbox(sb.clone()).only([]);
            cx.use_tool(mine.clone());
        });
        for agent in [without, left_out] {
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

#[tokio::test]
async fn each_instance_remembers_only_what_its_own_builtins_read() {
    let (folder, sb) = sandboxed().await;
    folder.write("a.txt", "one\n");
    let agent = agent(move |cx| {
        cx.use_sandbox(sb.clone());
    });
    let edit = || {
        [
            MockTurn::tool_call(
                "c1",
                "edit",
                json!({"path": "a.txt", "edits": [{"old_text": "one", "new_text": "1"}]}),
            ),
            MockTurn::text("Done."),
        ]
    };
    let (reader, other) = (Caches::default(), Caches::default());
    let read = [
        MockTurn::tool_call("c1", "read", json!({"path": "a.txt"})),
        MockTurn::text("Done."),
    ];
    scripted_in(&agent, read, &reader).await.unwrap();
    folder.write("a.txt", "two\n");

    let stale = "changed since it was last read";
    let theirs = results(&scripted_in(&agent, edit(), &other).await.unwrap());
    assert!(!theirs[0].0.contains(stale), "{theirs:?}");
    let mine = results(&scripted_in(&agent, edit(), &reader).await.unwrap());
    assert!(mine[0].0.contains(stale), "{mine:?}");
}
