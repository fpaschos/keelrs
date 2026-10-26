//! Flow: keelrs working on keelrs. A planner picks the next `agent-ready` issue, an implementer
//! commits it on a new branch, then the flow runs the repository's checks and a reviewer on
//! another model reads the change; failing checks or review findings go back to the same
//! implementer conversation, for at most three rounds. Approved work stays on the branch, and the
//! flow prints the commands that push it and open the pull request.
//!
//! No sandbox and no worktree: the agents work in this checkout, so start from a clean tree and
//! leave it alone while the flow runs. Each tool call is logged to stderr.
//!
//! Needs `ANTHROPIC_API_KEY`, and `gh` logged in. Run from anywhere in the repository with
//!
//! ```not_rust
//! cargo run -p example-dogfood
//! ```

mod coding;
mod report;

use std::process::Command;

use anyhow::{Context as _, bail, ensure};
use keelrs::{Agent, AgentContext, Runtime};
use rig_tools::OutputLimit;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::coding::CodingTools;
use crate::report::Report;

/// Implement-then-check-and-review rounds before the flow gives up.
const MAX_ROUNDS: usize = 3;

/// The checks `AGENTS.md` asks for, once per workspace.
const CHECKS: &[&str] = &[
    "cargo fmt --all --check",
    "cargo clippy --locked --workspace --all-targets --all-features -- -D warnings",
    "cargo test --locked --workspace",
    "cargo deny check",
    "cargo fmt --all --check --manifest-path examples/Cargo.toml",
    "cargo clippy --locked --workspace --all-targets --all-features --manifest-path examples/Cargo.toml -- -D warnings",
    "cargo test --locked --workspace --manifest-path examples/Cargo.toml",
    "cargo deny --manifest-path examples/Cargo.toml check",
];

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
    status: Status,
    /// What was implemented and committed, 1–5 lines.
    summary: String,
    /// What is left, when incomplete.
    remaining: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Status {
    Complete,
    Incomplete,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = run("git rev-parse --show-toplevel")?;
    std::env::set_current_dir(root.trim()).context("entering the repository")?;
    ensure!(
        run("git status --porcelain")?.trim().is_empty(),
        "the working tree has changes; commit or stash them first"
    );
    let base = run("git rev-parse --abbrev-ref HEAD")?.trim().to_owned();

    let tools = CodingTools::new();
    let plan = Report::<Plan>::new("report_plan");
    let implementation = Report::<Implementation>::new("report_implementation");
    let review = Report::<Review>::new("report_review");

    let planner = Agent::new("planner", {
        let (tools, plan) = (tools.clone(), plan.clone());
        move |cx: &mut AgentContext| {
            cx.use_model("anthropic/claude-sonnet-5-5");
            tools
                .looking()
                .into_iter()
                .for_each(|tool| cx.use_tool(tool));
            cx.use_tool(plan.tool("Report the issue you picked, or none. Call once, last."));
            include_str!("../prompts/plan.md").into()
        }
    });
    let implementer =
        Agent::new("implementer", {
            let (tools, implementation) = (tools.clone(), implementation.clone());
            move |cx: &mut AgentContext| {
                cx.use_model("anthropic/claude-sonnet-5-5");
                tools
                    .coding()
                    .into_iter()
                    .for_each(|tool| cx.use_tool(tool));
                cx.use_tool(implementation.tool(
                    "Report the outcome. Call once, as the very last step, after committing.",
                ));
                include_str!("../prompts/implement.md").into()
            }
        })
        .max_turns(300);
    let reviewer = Agent::new("reviewer", {
        let (tools, review) = (tools.clone(), review.clone());
        move |cx: &mut AgentContext| {
            cx.use_model("anthropic/claude-opus-5-5");
            tools
                .looking()
                .into_iter()
                .for_each(|tool| cx.use_tool(tool));
            cx.use_tool(review.tool("Report your verdict. Call once, last."));
            include_str!("../prompts/review.md").into()
        }
    });
    let runtime = Runtime::start([planner.clone(), implementer.clone(), reviewer.clone()]);

    // 1. Plan.
    let planned = runtime.init(&planner).id("plan")?;
    let Some(Picked { number, title }) = plan
        .ask(&planned, "Pick the next issue.".into())
        .await?
        .issue
    else {
        println!("Nothing to do: no agent-ready issue is unblocked.");
        return Ok(());
    };
    println!("Picked #{number}: {title}");

    // 2. Implement, check and review, sending failures back to the same conversation.
    let branch = format!("agent/issue-{number}");
    run(&format!("git switch -c {branch}"))?;
    let implementing = runtime
        .init(&implementer)
        .id(format!("implement-{number}"))?;
    let reviewing = runtime.init(&reviewer).id(format!("review-{number}"))?;
    let mut message = format!("Implement issue #{number}: {title}");
    for round in 1..=MAX_ROUNDS {
        println!("Round {round}: implementing");
        let report = implementation.ask(&implementing, message).await?;
        println!("{:?}: {}", report.status, report.summary);
        if report.status == Status::Incomplete {
            bail!(
                "#{number} is incomplete on {branch}: {}",
                report.remaining.unwrap_or_default()
            );
        }

        println!("Round {round}: checking");
        if let Some(failures) = failing_checks() {
            message = format!("The checks fail:\n\n{failures}\n\nFix them and commit.");
            continue;
        }

        println!("Round {round}: reviewing");
        let ask = format!("Review issue #{number}: the commits on `{branch}` since `{base}`.");
        let verdict = review.ask(&reviewing, ask).await?;
        if verdict.verdict == Verdict::Approve {
            println!("Approved. Push it and open the pull request with\n");
            println!("    git push -u origin {branch} && gh pr create --fill --base {base}");
            return Ok(());
        }
        println!("Changes asked: {}", verdict.findings.len());
        message = format!(
            "A reviewer asks for changes:\n\n- {}\n\nMake them and commit.",
            verdict.findings.join("\n- ")
        );
    }
    bail!("#{number} is not approved after {MAX_ROUNDS} rounds; the work is on {branch}")
}

/// The output of the first check that fails, its long output cut; `None` when all pass.
fn failing_checks() -> Option<String> {
    CHECKS.iter().find_map(|check| {
        let output = run(check).err()?.to_string();
        let (kept, _) = OutputLimit::default().tail(&output);
        Some(format!("`{check}`:\n\n{kept}"))
    })
}

/// Runs `command` with `sh -c`; its stdout, or an error with its stdout and stderr.
fn run(command: &str) -> anyhow::Result<String> {
    let output = Command::new("sh")
        .args(["-c", command])
        .output()
        .with_context(|| format!("starting `{command}`"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    ensure!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(stdout)
}
