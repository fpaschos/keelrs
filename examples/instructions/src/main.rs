//! Flow: an issue implementer whose instructions are a templated task, followed by a static
//! style guide added with `use_instruction`, answered by a real model.
//!
//! Needs `ANTHROPIC_API_KEY`. Run with
//!
//! ```not_rust
//! cargo run -p example-instructions
//! ```

use keelrs::{Agent, AgentContext, Runtime};

/// Runs again before every turn; the returned task comes first, then `style.md`.
fn implementer(cx: &mut AgentContext) -> String {
    cx.use_model("anthropic/claude-sonnet-5-5");
    cx.use_instruction(include_str!("../style.md"));
    include_str!("../task.md").replace("{{TOPIC}}", "add a `--verbose` flag to the CLI")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let implementer = Agent::new("implementer", implementer);
    let runtime = Runtime::start([implementer.clone()]);

    let instance = runtime.init(&implementer).id("demo")?;
    let reply = instance.ask("What is your plan?").await?;

    println!("{}", reply.text());
    Ok(())
}
