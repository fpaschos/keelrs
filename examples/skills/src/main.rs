//! Flow: mount a skill. SAMPLE for the open `use_skill` question; not decided API.
//!
//! The skill is resolved before the agent function runs: `skill!` embeds
//! `SKILL.md` at compile time, like Flue's `import greeting from './SKILL.md'`.
//! The function only declares it.
//!
//! Run with
//!
//! ```not_rust
//! cargo run -p example-skills
//! ```

use keelrs::{Agent, AgentContext, Runtime, Skill};

/// Embedded at compile time; the path is relative to this file.
static GREETING: Skill = keelrs::skill!("../skills/greeting/SKILL.md");

fn greeter(cx: &mut AgentContext) -> String {
    cx.use_model("anthropic/claude-sonnet-5-5");
    // STUB: accepted and ignored until the skill design is decided.
    cx.use_skill(GREETING);
    "Activate the `greeting` skill before greeting anyone.".into()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("mounted: {} — {}", GREETING.name(), GREETING.description());

    let greeter = Agent::new("greeter", greeter);
    let runtime = Runtime::start([greeter.clone()]);

    let instance = runtime.init(&greeter).id("demo");
    let receipt = instance.dispatch("Hi, I'm Ada").await?;
    let reply = instance.read(&receipt).await?;

    println!("{}", reply.text());
    Ok(())
}
