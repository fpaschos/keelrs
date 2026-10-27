//! Flow: keelrs working on keelrs. A planner picks the next `agent-ready` issue, an implementer
//! changes the working tree, and a reviewer on another model reads the change; its findings go
//! back to the same implementer conversation, until it approves or [`MAX_ROUNDS`] run out.
//! Nothing is committed.
//!
//! The agents share one local sandbox rooted at this checkout (`keelrs::sandbox::local`): their
//! file tools stay inside it, and `shell` starts there from an env allowlist (so `gh` must be
//! logged in through its config, not `GH_TOKEN`). No worktree. Each tool call is logged to stderr.
//!
//! Needs `ANTHROPIC_API_KEY`, and `gh` logged in. Run from the repository root with
//!
//! ```not_rust
//! cargo run --manifest-path examples/Cargo.toml -p example-dogfood
//! ```

mod coding;
mod report;

use std::sync::LazyLock;

use anyhow::bail;
use keelrs::sandbox::{self, Sandbox};
use keelrs::{Agent, AgentContext, Runtime};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::coding::{CODING, LOOKING};
use crate::report::Report;

/// Implement-then-review rounds before the flow gives up.
const MAX_ROUNDS: usize = 3;

/// The planner's pick.
#[derive(Debug, Deserialize, JsonSchema)]
struct Plan {
    /// The issue to implement next; null when no issue is ready.
    issue: Option<Picked>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Picked {
    number: u64,
    title: String,
}

/// The implementer's report.
#[derive(Debug, Deserialize, JsonSchema)]
struct Implementation {
    /// What was changed, 1–5 lines.
    summary: String,
}

/// The reviewer's verdict.
#[derive(Debug, Deserialize, JsonSchema)]
struct Review {
    verdict: Verdict,
    /// One actionable sentence per finding, naming the file; empty when approved.
    findings: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Approve,
    Changes,
}

static PLAN: LazyLock<Report<Plan>> = LazyLock::new(|| Report::new("report_plan"));
static IMPLEMENTATION: LazyLock<Report<Implementation>> =
    LazyLock::new(|| Report::new("report_implementation"));
static REVIEW: LazyLock<Report<Review>> = LazyLock::new(|| Report::new("report_review"));

/// Picks the next issue, looking only.
fn planner(cx: &mut AgentContext, sandbox: &Sandbox) -> String {
    cx.use_sandbox(sandbox.clone());
    cx.use_model("anthropic/claude-sonnet-5-5");
    for tool in LOOKING.iter() {
        cx.use_tool(tool.clone());
    }
    cx.use_tool(PLAN.tool("Report the issue you picked, or none. Call once, last."));
    include_str!("../prompts/plan.md").into()
}

/// Changes the working tree for one issue.
fn implementer(cx: &mut AgentContext, sandbox: &Sandbox) -> String {
    cx.use_sandbox(sandbox.clone());
    cx.use_model("anthropic/claude-sonnet-5-5");
    for tool in CODING.iter() {
        cx.use_tool(tool.clone());
    }
    cx.use_tool(IMPLEMENTATION.tool("Report the outcome. Call once, as the very last step."));
    include_str!("../prompts/implement.md").into()
}

/// Reviews the change on another model, looking only.
fn reviewer(cx: &mut AgentContext, sandbox: &Sandbox) -> String {
    cx.use_sandbox(sandbox.clone());
    cx.use_model("anthropic/claude-opus-5-5");
    for tool in LOOKING.iter() {
        cx.use_tool(tool.clone());
    }
    cx.use_tool(REVIEW.tool("Report your verdict. Call once, last."));
    include_str!("../prompts/review.md").into()
}

/// An agent function over its own clone of `sandbox`.
fn agent(
    sandbox: &Sandbox,
    declare: fn(&mut AgentContext, &Sandbox) -> String,
) -> impl Fn(&mut AgentContext) -> String + Send + Sync + 'static {
    let sandbox = sandbox.clone();
    move |cx| declare(cx, &sandbox)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Rooted at the directory it runs from: the repository root.
    let sandbox = sandbox::local().open().await?;
    let planner = Agent::new("planner", agent(&sandbox, planner));
    let implementer = Agent::new("implementer", agent(&sandbox, implementer)).max_turns(300);
    let reviewer = Agent::new("reviewer", agent(&sandbox, reviewer));
    let runtime = Runtime::start([planner.clone(), implementer.clone(), reviewer.clone()]);

    // 1. Plan.
    let planned = runtime.init(&planner).id("plan")?;
    let Some(Picked { number, title }) = PLAN
        .ask(&planned, "Pick the next issue.".into())
        .await?
        .issue
    else {
        println!("Nothing to do: no agent-ready issue is unblocked.");
        return Ok(());
    };
    println!("Picked #{number}: {title}");

    // 2. Implement and review, sending findings back to the same implementer conversation.
    let implementing = runtime
        .init(&implementer)
        .id(format!("implement-{number}"))?;
    let reviewing = runtime.init(&reviewer).id(format!("review-{number}"))?;
    let mut message = format!("Implement issue #{number}: {title}");
    for round in 1..=MAX_ROUNDS {
        println!("Round {round}: implementing");
        let implementation = IMPLEMENTATION.ask(&implementing, message).await?;
        println!("{}", implementation.summary);

        println!("Round {round}: reviewing");
        let ask = format!("Review issue #{number}: the uncommitted changes.");
        let review = REVIEW.ask(&reviewing, ask).await?;
        if review.verdict == Verdict::Approve {
            println!("Approved.");
            return Ok(());
        }
        message = format!(
            "A reviewer asks for changes:\n\n- {}\n\nMake them.",
            review.findings.join("\n- ")
        );
        println!("{message}");
    }
    bail!("#{number} is not approved after {MAX_ROUNDS} rounds")
}
