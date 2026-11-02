//! Flow: the smallest possible agent — one turn, no tools — answered by a real model.
//!
//! Needs `ANTHROPIC_API_KEY`. Run with
//!
//! ```not_rust
//! cargo run -p example-hello-world
//! ```

use keelrs::{Agent, AgentContext, Runtime, sandbox};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sandbox = sandbox::local().open().await?;
    // Runs again before every turn; returns synchronously.
    let hello = Agent::new("hello", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        // Every agent declares a sandbox; this one keeps none of its built-in tools.
        cx.use_sandbox(sandbox.clone()).only([]);
        "You greet people warmly, in one sentence.".into()
    });
    let runtime = Runtime::start([hello.clone()]);

    let instance = runtime.init(&hello).id("demo")?;
    let reply = instance.ask("Say hello").await?;

    println!("{}", reply.text());
    Ok(())
}
