//! Flow: keelrs working on keelrs, in a sandbox it reaches only by bytes. The host seeds `main`
//! into the sandbox as a bundle; a planner picks the next `agent-ready` issue; an implementer
//! changes the clone and a reviewer on another model reads the change, its findings going back
//! to the same implementer conversation, until it approves or [`MAX_ROUNDS`] run out. The flow
//! then commits, harvests the branch as a bundle, pushes it and opens a pull request.
//!
//! The sandbox is `keelrs::sandbox::local`, rooted at a scratch directory standing in for a
//! remote one: [`git::seed`], [`git::harvest`] and `shell` reach it only through `Sandbox`, so
//! they work on any backend. The file tools in [`coding`] are still rig-tools' stand-ins on this
//! machine's disk, until keelrs has its own (#42). GitHub stays on the host: agents read issues
//! through the `issues` and `issue` host tools, and the flow pushes. Each tool call is logged to
//! stderr.
//!
//! Needs `ANTHROPIC_API_KEY`, and `gh` logged in. Run from the repository root with
//!
//! ```not_rust
//! cargo run --manifest-path examples/Cargo.toml -p example-build-review-loop
//! ```

mod coding;
mod git;
mod host;
mod report;

use std::path::PathBuf;
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
    cx.use_tool(host::ISSUES.clone());
    cx.use_tool(host::ISSUE.clone());
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
    cx.use_tool(host::ISSUE.clone());
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
    cx.use_tool(host::ISSUE.clone());
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

/// The sandbox's root: emptied on each run. The only line a remote backend changes is the
/// `open` below, which then needs no directory on this machine.
fn scratch() -> std::io::Result<PathBuf> {
    let root = std::env::temp_dir().join("keelrs-build-review-loop");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sandbox = sandbox::local().root(scratch()?).open().await?;
    git::seed(&sandbox, "main").await?;
    let work = sandbox.work_dir(git::WORK);

    let planner = Agent::new("planner", agent(&work, planner));
    let implementer = Agent::new("implementer", agent(&work, implementer)).max_turns(300);
    let reviewer = Agent::new("reviewer", agent(&work, reviewer));
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
    let branch = format!("keelrs/issue-{number}");
    git::run(&work, ["git", "switch", "--create", branch.as_str()]).await?;

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
            // 3. Commit, harvest and open the pull request, on the host.
            git::commit_all(&work, &format!("#{number}: {title}")).await?;
            let bundle = git::harvest(&sandbox, "main", &branch).await?;
            let body = format!("Closes #{number}.\n\n{}", implementation.summary);
            let url = host::open_pr(bundle, &branch, &title, &body).await?;
            println!("Opened {url}");
            sandbox.close().await?;
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
