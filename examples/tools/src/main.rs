//! Flow: an agent with two host tools and one sandboxed tool, answered by a real model.
//! `current_time` takes no arguments, `get_weather` calls an online API, and `pwd` runs in the
//! sandbox the agent declares: a local one rooted at this example and narrowed to its `src`, so
//! it answers with that directory. Each tool lives in its own module.
//!
//! Needs `ANTHROPIC_API_KEY`, and the network for `get_weather` (Open-Meteo, no key). Run with
//!
//! ```not_rust
//! cargo run -p example-tools
//! ```

mod failure;
mod pwd;
mod time;
mod weather;

use keelrs::sandbox;
use keelrs::{Agent, AgentContext, Runtime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // This machine, rooted at this example's folder; the agent works in its `src`.
    let sandbox = sandbox::local()
        .root(env!("CARGO_MANIFEST_DIR"))
        .open()
        .await?
        .work_dir("src");
    let http = reqwest::Client::new();

    let assistant = Agent::new("assistant", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        // None of keelrs's built-in tools: `pwd` is this example's one sandboxed tool.
        cx.use_sandbox(sandbox.clone()).only([]);
        cx.use_tool(time::current_time());
        cx.use_tool(weather::get_weather(http.clone()));
        cx.use_tool(pwd::pwd());
        "You answer questions with your tools; never guess a fact a tool can give you.".into()
    });
    let runtime = Runtime::start([assistant.clone()]);

    let reply = runtime
        .init(&assistant)
        .id("demo")?
        .ask("What time is it, what's the weather in Athens, and which directory do you work in?")
        .await?;
    println!("{}", reply.text());
    Ok(())
}
