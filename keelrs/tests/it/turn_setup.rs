//! A turn setup that declares something wrong fails its dispatch, through the public API, with
//! no network.

use std::convert::Infallible;

use keelrs::sandbox::{self, Listing, Sandbox};
use keelrs::{Agent, AgentContext, Error, InstanceId, Outcome, Runtime, Skill, Tool, ToolCall};

/// Why a fresh instance of `agent` fails, asked plain and structured: both must fail alike.
async fn failure_reason(agent: Agent) -> String {
    let runtime = Runtime::start([&agent]).unwrap();
    let instance = runtime.instance(&agent, InstanceId::new("test")).unwrap();
    let plain = instance.ask("Say hello").await.map(drop);
    let structured = instance
        .ask_structured::<String>("Say hello")
        .await
        .map(drop);
    let reasons = [plain, structured].map(|result| match result {
        Err(Error::Run {
            outcome: Outcome::Failed { reason },
            ..
        }) => reason,
        other => panic!("expected a failed run, got {other:?}"),
    });
    assert_eq!(reasons[0], reasons[1]);
    reasons[0].clone()
}

/// A valid model and a sandbox with no built-ins: what most rows add their one mistake to.
fn valid(cx: &mut AgentContext, sb: &Sandbox) {
    cx.use_model("anthropic/claude-sonnet-5-5");
    cx.use_sandbox(sb).only([]);
}

fn named(name: &str) -> Tool {
    Tool::host(name.to_owned(), "Mine.", async |_: ToolCall<()>| {
        Ok::<_, Infallible>("")
    })
}

static REVIEW: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/review");
static NO_SKILL_MD: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/no-skill-md");
static SECRET: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/with-secret");
static CLUTTER: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/with-clutter");

type Declare = fn(&mut AgentContext, &Sandbox);

#[tokio::test]
async fn a_wrong_declaration_fails_the_dispatch_with_its_reason() {
    let cases: [(&str, Declare); 16] = [
        // A missing model is reported before a missing sandbox.
        ("the agent function never called `use_model`", |_, _| {}),
        (
            "`use_model` was called 2 times in one turn setup; call it once",
            |cx, sb| {
                valid(cx, sb);
                cx.use_model("anthropic/claude-opus-5-5");
            },
        ),
        ("`use_model` was given an empty model id", |cx, _| {
            cx.use_model("")
        }),
        (
            "the catalog lists no model `nope/model`; did you mean…",
            |cx, sb| {
                cx.use_model("nope/model");
                cx.use_sandbox(sb).only([]);
            },
        ),
        ("`use_instruction` was given empty text", |cx, sb| {
            valid(cx, sb);
            cx.use_instruction("  ");
        }),
        ("the agent function never called `use_sandbox`", |cx, _| {
            cx.use_model("anthropic/claude-sonnet-5-5");
            cx.use_tool(Tool::sandbox(
                "list_files",
                "The files in the work dir.",
                async |_: ToolCall<()>, sb: Sandbox| sb.list(".", None, Listing::Visible).await,
            ));
        }),
        (
            "`use_sandbox` was called 2 times in one turn setup; call it once",
            |cx, sb| {
                valid(cx, sb);
                cx.use_sandbox(sb);
            },
        ),
        (
            "two tools are named `clock`; tool names must be unique",
            |cx, sb| {
                valid(cx, sb);
                cx.use_tool(named("clock"));
                cx.use_tool(named("clock"));
            },
        ),
        (
            "`echo`'s arguments must be a JSON object; its schema is {\"type\":\"string\"}",
            |cx, sb| {
                valid(cx, sb);
                cx.use_tool(Tool::host(
                    "echo",
                    "Echo the text.",
                    async |call: ToolCall<String>| Ok::<_, Infallible>(call.data),
                ));
            },
        ),
        (
            "`finish_structured` is reserved for keelrs's built-in tool",
            |cx, sb| {
                valid(cx, sb);
                cx.use_tool(named("finish_structured"));
            },
        ),
        (
            "`activate_skill` is reserved for keelrs's built-in tool",
            |cx, sb| {
                valid(cx, sb);
                cx.use_tool(named("activate_skill"));
            },
        ),
        (
            "`read_skill_resource` is reserved for keelrs's built-in tool",
            |cx, sb| {
                valid(cx, sb);
                cx.use_tool(named("read_skill_resource"));
            },
        ),
        (
            "two skills are named `review`; skill names must be unique",
            |cx, sb| {
                valid(cx, sb);
                let haiku = Skill::new("review", "Write a haiku.", "Body.").unwrap();
                cx.use_skills([&REVIEW, &haiku]);
            },
        ),
        (
            "skill `no-skill-md`: the folder has no `SKILL.md`",
            |cx, sb| {
                valid(cx, sb);
                cx.use_skill(&NO_SKILL_MD);
            },
        ),
        (
            "skill `with-secret`: `secrets.txt` would be embedded into the binary; remove it from the skill's folder",
            |cx, sb| {
                valid(cx, sb);
                cx.use_skill(&SECRET);
            },
        ),
        (
            "skill `with-clutter`: `node_modules` would be embedded into the binary; remove it from the skill's folder",
            |cx, sb| {
                valid(cx, sb);
                cx.use_skill(&CLUTTER);
            },
        ),
    ];
    let sb = sandbox::local()
        .root(std::env::temp_dir())
        .open()
        .await
        .unwrap();
    for (expected, declare) in cases {
        let sb = sb.clone();
        let agent = Agent::new("wrong", move |cx: &mut AgentContext| {
            declare(cx, &sb);
            "Help.".into()
        });
        let reason = failure_reason(agent).await;
        // A reason ending in `…` goes on with text the model catalog decides.
        match expected.strip_suffix('…') {
            Some(start) => assert!(reason.starts_with(start), "{reason:?}"),
            None => assert_eq!(reason, expected),
        }
    }
}
