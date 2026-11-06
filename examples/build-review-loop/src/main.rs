//! Flow: keelrs working on keelrs, in a sandbox it reaches only by bytes. The host seeds `main`
//! into the sandbox as a bundle; a planner picks the next `agent-ready` issue; an implementer
//! changes the clone and a reviewer on another model reads the change, its findings going back
//! to the same implementer conversation, until it approves or [`MAX_ROUNDS`] run out. The flow
//! then commits, harvests the branch as a bundle, pushes it and opens a pull request.
//!
//! The sandbox is `keelrs::sandbox::local`, rooted at a scratch directory standing in for a
//! remote one: [`git::seed`], [`git::harvest`] and keelrs's built-in tools reach it only
//! through `Sandbox`, so they work on any backend. GitHub stays on the host, opened as a trusted
//! `Sandbox` with this machine's environment (`inherit_env`): agents read issues through the
//! `issues` and `issue` host tools, and the flow pushes.
//!
//! Needs `ANTHROPIC_API_KEY`, and `gh` logged in. Run from the repository root with
//!
//! ```not_rust
//! cargo run --manifest-path examples/Cargo.toml -p example-build-review-loop
//! ```

mod git;
mod host;

use std::path::PathBuf;

use anyhow::bail;
use keelrs::sandbox::{self, Sandbox};
use keelrs::{Agent, AgentContext, InstanceId, Runtime};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Implement-then-review rounds before the flow gives up.
const MAX_ROUNDS: usize = 3;

/// The planner's pick.
#[derive(Debug, Deserialize, JsonSchema)]
struct Plan {
    /// The issue to implement next; null when no issue is ready.
    issue: Option<Issue>,
}

/// The issue an implementer and a reviewer work on: their instance data.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct Issue {
    number: u64,
    title: String,
}

/// The implementer's outcome.
#[derive(Debug, Deserialize, JsonSchema)]
struct Implementation {
    /// What was changed, 1–5 lines, naming any changes you found and left as they were.
    summary: String,
}

/// The reviewer's verdict.
#[derive(Debug, Deserialize, JsonSchema)]
struct Review {
    /// `approve`, or `changes` when there is at least one finding.
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

/// Picks the next issue, looking only.
fn planner(cx: &mut AgentContext, sandbox: &Sandbox, host: &Sandbox) -> String {
    cx.use_sandbox(sandbox)
        .only(["read", "grep", "glob", "bash"]);
    cx.use_model("anthropic/claude-sonnet-5-5");
    cx.use_tools([host::issues(host), host::issue(host)]);
    include_str!("../prompts/plan.md").into()
}

/// Changes the working tree for its issue.
fn implementer(cx: &mut AgentContext<Issue>, sandbox: &Sandbox, host: &Sandbox) -> String {
    let Issue { number, title } = cx.data();
    let task = format!("You implement #{number}: {title}.");
    cx.use_sandbox(sandbox);
    cx.use_model("anthropic/claude-sonnet-5-5");
    cx.use_tool(host::issue(host));
    format!("{}\n\n{task}", include_str!("../prompts/implement.md"))
}

/// Reviews the change to its issue on another model, looking only.
fn reviewer(cx: &mut AgentContext<Issue>, sandbox: &Sandbox, host: &Sandbox) -> String {
    let Issue { number, title } = cx.data();
    let task = format!("You review #{number}: {title}.");
    cx.use_sandbox(sandbox)
        .only(["read", "grep", "glob", "bash"]);
    cx.use_model("anthropic/claude-opus-5-5");
    cx.use_tool(host::issue(host));
    format!("{}\n\n{task}", include_str!("../prompts/review.md"))
}

/// An agent function over its own clones of `sandbox` and `host`.
fn agent<D: 'static>(
    sandbox: &Sandbox,
    host: &Sandbox,
    declare: fn(&mut AgentContext<D>, &Sandbox, &Sandbox) -> String,
) -> impl Fn(&mut AgentContext<D>) -> String + Send + Sync + 'static {
    let (sandbox, host) = (sandbox.clone(), host.clone());
    move |cx| declare(cx, &sandbox, &host)
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
    let host = sandbox::local().inherit_env().open().await?;
    let sandbox = sandbox::local().root(scratch()?).open().await?;
    git::seed(&host, &sandbox, "main").await?;
    let work = sandbox.work_dir(git::WORK);

    let planner = Agent::new("planner", agent(&work, &host, planner));
    let implementer = Agent::new("implementer", agent(&work, &host, implementer)).max_turns(300);
    let reviewer = Agent::new("reviewer", agent(&work, &host, reviewer));
    let runtime = Runtime::start((&planner, &implementer, &reviewer))?;

    // 1. Plan.
    let planned = runtime.instance(&planner, InstanceId::fresh())?;
    let Some(issue) = planned
        .ask_structured::<Plan>("Pick the next issue.")
        .await?
        .issue
    else {
        println!("Nothing to do: no agent-ready issue is unblocked.");
        return Ok(());
    };

    let Issue { number, title } = &issue;
    println!("Picked #{number}: {title}");
    let branch = format!("keelrs/issue-{number}");
    work.run(["git", "switch", "--create", &branch]).await?;

    // 2. Implement and review, sending findings back to the same implementer conversation.
    // Each conversation is about this issue: its instance data.
    let implementing = runtime.instance_with(
        &implementer,
        InstanceId::new(format!("implement-{number}")),
        issue.clone(),
    )?;

    let reviewing = runtime.instance_with(
        &reviewer,
        InstanceId::new(format!("review-{number}")),
        issue.clone(),
    )?;

    let mut ask = "Implement the issue.".to_owned();
    for round in 1..=MAX_ROUNDS {
        println!("Round {round}: implementing");
        let implementation = implementing.ask_structured::<Implementation>(&ask).await?;
        println!("{}", implementation.summary);

        println!("Round {round}: reviewing");
        let review = reviewing
            .ask_structured::<Review>("Review the uncommitted changes.")
            .await?;
        if review.verdict == Verdict::Approve {
            println!("Approved.");
            // 3. Commit, harvest and open the pull request, on the host.
            git::commit_all(&work, &format!("#{number}: {title}")).await?;
            let bundle = git::harvest(&sandbox, "main", &branch).await?;
            let body = format!("Closes #{number}.\n\n{}", implementation.summary);
            let url = host::open_pr(&host, bundle, &branch, title, &body).await?;
            println!("Opened {url}");
            sandbox.close().await?;
            return Ok(());
        }
        ask = format!(
            "A reviewer asks for changes:\n\n- {}\n\nMake them.",
            review.findings.join("\n- ")
        );
        println!("{ask}");
    }
    bail!("#{number} is not approved after {MAX_ROUNDS} rounds")
}
