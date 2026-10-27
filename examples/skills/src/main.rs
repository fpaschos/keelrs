//! Flow: skills from two sources, one dispatch each, answered by a real model.
//!
//! 1. `greeting` is embedded at compile time with `skill!`, like Flue's
//!    `import greeting from './SKILL.md'`: the whole folder, so also
//!    `references/tone.md`, which its instructions tell the model to read. The
//!    model calls `activate_skill`, then `read_skill_resource`.
//! 2. `haiku` is built in code with `Skill::new`, like Flue's `defineSkill`,
//!    and mounted with `use_skills` beside `greeting`.
//!
//! The model sees only each skill's name and description until it activates one.
//! Needs `ANTHROPIC_API_KEY`. Run with
//!
//! ```not_rust
//! cargo run -p example-skills
//! ```

use keelrs::{Agent, AgentContext, Runtime, Skill};

/// The whole folder, embedded at compile time; a broken skill fails its first dispatch.
static GREETING: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/skills/greeting");

fn greeter(cx: &mut AgentContext) -> String {
    cx.use_model("anthropic/claude-sonnet-5-5");
    cx.use_skill(&GREETING);
    "You greet people.".into()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let haiku = Skill::new(
        "haiku",
        "Write a haiku about a subject. Use when asked for a haiku or a very short poem.",
        "Write exactly three lines of 5, 7 and 5 syllables. No title, no explanation.",
    )?;

    let greeter = Agent::new("greeter", greeter);
    let poet = Agent::new("poet", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_skills([&GREETING, &haiku]);
        "You greet people and write poems.".into()
    });
    let runtime = Runtime::start([greeter.clone(), poet.clone()]);

    println!(
        "1. embedded: {} — {}",
        GREETING.name(),
        GREETING.description()
    );
    let reply = runtime
        .init(&greeter)
        .id("demo")?
        .ask("Hi, I'm Ada")
        .await?;
    println!("{}\n", reply.text());

    println!("2. built in code, beside the embedded one: haiku, greeting");
    let reply = runtime
        .init(&poet)
        .id("demo")?
        .ask("A haiku about the sea, please.")
        .await?;
    println!("{}", reply.text());
    Ok(())
}
