//! Flow: keelrs working on keelrs. A planner picks the next `agent-ready` issue, an implementer
//! changes the working tree, and a reviewer on another model reads the change; its findings go
//! back to the same implementer conversation, for at most three rounds. Nothing is committed.
//!
//! No sandbox and no worktree: the agents work in this checkout. Each tool call is logged to
//! stderr.
//!
//! Needs `ANTHROPIC_API_KEY`, and `gh` logged in. Run from the repository root with
//!
//! ```not_rust
//! cargo run --manifest-path examples/Cargo.toml -p example-dogfood
//! ```

mod coding;
mod report;

use anyhow::bail;
use keelrs::{Agent, AgentContext, Runtime};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::coding::CodingTools;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tools = CodingTools::new();
    let plan = Report::<Plan>::new("report_plan");
    let implementation = Report::<Implementation>::new("report_implementation");
    let review = Report::<Review>::new("report_review");

    let planner = Agent::new("planner", {
        let (tools, plan) = (tools.clone(), plan.clone());
        move |cx: &mut AgentContext| {
            cx.use_model("anthropic/claude-sonnet-5-5");
            for tool in tools.looking() {
                cx.use_tool(tool);
            }
            cx.use_tool(plan.tool("Report the issue you picked, or none. Call once, last."));
            include_str!("../prompts/plan.md").into()
        }
    });
    let implementer = Agent::new("implementer", {
        let (tools, implementation) = (tools.clone(), implementation.clone());
        move |cx: &mut AgentContext| {
            cx.use_model("anthropic/claude-sonnet-5-5");
            for tool in tools.coding() {
                cx.use_tool(tool);
            }
            cx.use_tool(
                implementation.tool("Report the outcome. Call once, as the very last step."),
            );
            include_str!("../prompts/implement.md").into()
        }
    })
    .max_turns(300);

    let reviewer = Agent::new("reviewer", {
        let (tools, review) = (tools.clone(), review.clone());
        move |cx: &mut AgentContext| {
            cx.use_model("anthropic/claude-opus-5-5");
            for tool in tools.looking() {
                cx.use_tool(tool);
            }
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

    // 2. Implement and review, sending findings back to the same conversation.
    let implementing = runtime
        .init(&implementer)
        .id(format!("implement-{number}"))?;
    let reviewing = runtime.init(&reviewer).id(format!("review-{number}"))?;
    let mut message = format!("Implement issue #{number}: {title}");
    for round in 1..=MAX_ROUNDS {
        println!("Round {round}: implementing");
        let report = implementation.ask(&implementing, message).await?;
        println!("{}", report.summary);

        println!("Round {round}: reviewing");
        let ask = format!("Review issue #{number}: the uncommitted changes.");
        let verdict = review.ask(&reviewing, ask).await?;
        if verdict.verdict == Verdict::Approve {
            println!("Approved.");
            return Ok(());
        }
        println!("Changes asked: {}", verdict.findings.len());
        message = format!(
            "A reviewer asks for changes:\n\n- {}\n\nMake them.",
            verdict.findings.join("\n- ")
        );
    }
    bail!("#{number} is not approved after {MAX_ROUNDS} rounds")
}
