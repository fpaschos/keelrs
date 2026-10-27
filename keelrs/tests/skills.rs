//! Building and mounting skills, through the public API, with no network.

use std::convert::Infallible;

use keelrs::{Agent, AgentContext, Error, Outcome, Runtime, Skill, Tool, ToolCall};

/// Asks a fresh instance of `agent` one message and returns its failure reason.
async fn failure_reason(agent: Agent) -> String {
    let runtime = Runtime::start([agent.clone()]);
    let instance = runtime.init(&agent).id("test").unwrap();
    match instance.ask("Say hello").await {
        Err(Error::Run {
            outcome: Outcome::Failed { reason },
            ..
        }) => reason,
        other => panic!("expected a failed run, got {other:?}"),
    }
}

fn agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    Agent::new("skilled", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        declare(cx);
        "Help.".into()
    })
}

#[tokio::test]
async fn a_tool_with_a_reserved_name_fails() {
    for name in [
        "activate_skill",
        "read_skill_resource",
        "task",
        "finish",
        "give_up",
    ] {
        let agent = agent(move |cx| {
            cx.use_tool(Tool::new(name, "Mine.", async |_: ToolCall<()>| {
                Ok::<_, Infallible>("")
            }));
        });
        assert_eq!(
            failure_reason(agent).await,
            format!("`{name}` is reserved for keelrs's built-in tool")
        );
    }
}

#[tokio::test]
async fn two_skills_with_one_name_fail() {
    let agent = agent(|cx| {
        let haiku = Skill::new("review", "Write a haiku.", "Body.").unwrap();
        cx.use_skills([&REVIEW, &haiku]);
    });
    assert_eq!(
        failure_reason(agent).await,
        "two skills are named `review`; skill names must be unique"
    );
}

#[tokio::test]
async fn an_invalid_embedded_skill_fails_its_dispatch() {
    static EXTRA: Skill = keelrs::skill!("fixtures/skills/extra-field");
    let agent = agent(|cx| cx.use_skill(&EXTRA));
    let reason = failure_reason(agent).await;
    assert!(reason.starts_with("skill `extra-field`: "), "{reason}");
}

#[test]
fn a_code_built_skill_exposes_its_frontmatter() {
    let skill = Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap();
    assert_eq!(skill.name(), "haiku");
    assert_eq!(skill.description(), "Write a haiku.");
    assert_eq!(skill.license(), None);
    assert!(skill.metadata().is_empty());
    assert_eq!(skill.allowed_tools().count(), 0);
    assert_eq!(skill.validate(), Ok(()));
}

/// Why `Skill::new(name, …)` refuses, as text.
fn refused_name(name: &str) -> String {
    Skill::new(name, "Something.", "Body.")
        .unwrap_err()
        .to_string()
}

#[test]
fn a_code_built_skill_name_follows_the_spec() {
    let long = "a".repeat(65);
    for name in ["", "Haiku", "-haiku", "haiku-", "hai--ku", "hai_ku", &long] {
        assert!(
            refused_name(name).contains("`name`"),
            "{name:?} was accepted"
        );
    }
}

#[test]
fn a_code_built_skill_needs_a_description() {
    let error = Skill::new("haiku", "  ", "Body.").unwrap_err().to_string();
    assert_eq!(error, "skill `haiku`: `description` must not be empty");
}

#[test]
fn a_code_built_skill_file_stays_inside_its_folder() {
    let skill = || Skill::new("haiku", "Write a haiku.", "Body.").unwrap();
    for path in [
        "/etc/passwd",
        "../secret.md",
        "references/../../x",
        "",
        "SKILL.md",
    ] {
        assert!(skill().file(path, "x").is_err(), "{path:?} was accepted");
    }
    assert!(skill().file("references/forms.md", "x").is_ok());
}

static REVIEW: Skill = keelrs::skill!("fixtures/skills/review", files: ["references/style.md"]);

#[test]
fn an_embedded_skill_exposes_all_six_fields() {
    assert_eq!(REVIEW.validate(), Ok(()));
    assert_eq!(REVIEW.name(), "review");
    assert_eq!(REVIEW.description(), "Review a change for style.");
    assert_eq!(REVIEW.license(), Some("MIT"));
    assert_eq!(REVIEW.compatibility(), Some("Needs nothing."));
    assert_eq!(REVIEW.metadata()["author"], "keelrs");
    assert_eq!(REVIEW.allowed_tools().collect::<Vec<_>>(), ["read", "grep"]);
}

#[test]
fn an_embedded_skill_with_a_field_beyond_the_spec_is_invalid() {
    let skill: Skill = keelrs::skill!("fixtures/skills/extra-field");
    let error = skill.validate().unwrap_err().to_string();
    assert!(error.contains("argument-hint"), "{error}");
    assert_eq!(skill.name(), "extra-field");
}

#[test]
fn an_embedded_skill_name_must_match_its_folder() {
    let skill: Skill = keelrs::skill!("fixtures/skills/wrong-folder");
    assert_eq!(
        skill.validate().unwrap_err().to_string(),
        "skill `wrong-folder`: `name` is `right-name` but the folder is `wrong-folder`; they must match"
    );
}
