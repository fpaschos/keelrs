//! Flow: an issue implementer whose instructions are a templated task, followed by a static
//! style guide added with `use_instruction`, answered by a real model.
//!
//! Needs `ANTHROPIC_API_KEY`. Run with
//!
//! ```not_rust
//! cargo run -p example-instructions
//! ```

use keelrs::{Agent, AgentContext, InstanceId, Runtime, sandbox};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sandbox = sandbox::local().open().await?;
    // Runs again before every turn; the returned task comes first, then `style.md`.
    let implementer = Agent::new("implementer", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        // Only plans: none of keelrs's built-in tools.
        cx.use_sandbox(&sandbox).only([]);
        cx.use_instruction(include_str!("../style.md"));
        include_str!("../task.md").replace("{{TOPIC}}", "add a `--verbose` flag to the CLI")
    });
    let runtime = Runtime::start([&implementer])?;

    let instance = runtime.instance(&implementer, InstanceId::new("demo"))?;
    let reply = instance.ask("What is your plan?").await?;

    println!("{}", reply.text());
    Ok(())
}
