//! Flow: the smallest possible agent — one turn, no tools.
//!
//! Run with
//!
//! ```not_rust
//! cargo run -p example-hello-world
//! ```

use keelrs::{Agent, AgentContext, Runtime};

/// Runs again before every turn; returns synchronously.
fn hello(cx: &mut AgentContext) -> String {
    cx.use_model("anthropic/claude-sonnet-5-5");
    "You greet people warmly, in one sentence.".into()
}

#[tokio::main]
async fn main() -> keelrs::Result<()> {
    let hello = Agent::new("hello", hello);
    let runtime = Runtime::start([hello.clone()]);

    let instance = runtime.init(&hello).id("demo");
    let receipt = instance.dispatch("Say hello").await?;
    let reply = instance.read(&receipt).await?;

    println!("{}", reply.text());
    Ok(())
}
