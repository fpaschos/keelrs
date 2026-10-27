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
    for name in ["activate_skill", "read_skill_resource"] {
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

static REVIEW: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/review");

#[test]
fn an_embedded_skill_reads_its_name_and_description() {
    assert_eq!(REVIEW.name(), "review");
    assert_eq!(REVIEW.description(), "Review a change for style.");
}

#[test]
fn frontmatter_fields_beyond_name_and_description_are_ignored() {
    let skill: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/extra-field");
    assert_eq!(skill.description(), "Has fields keelrs does not read.");
}

#[test]
fn an_embedded_skill_reads_an_unquoted_value_with_a_colon_as_text() {
    let skill: Skill =
        keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/colon-description");
    assert_eq!(
        skill.description(),
        "Review code. Use when: the user asks for a review."
    );
}

#[test]
fn an_embedded_skill_may_hold_templates_named_like_secrets() {
    let skill: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/with-templates");
    assert_eq!(skill.description(), "Set up a project's environment.");
}

/// Why mounting the embedded `skill` fails its dispatch.
async fn mount_failure(skill: &'static Skill) -> String {
    failure_reason(agent(move |cx| cx.use_skill(skill))).await
}

#[tokio::test]
async fn an_embedded_folder_without_skill_md_fails_its_dispatch() {
    static NONE: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/no-skill-md");
    assert_eq!(
        mount_failure(&NONE).await,
        "skill `no-skill-md`: the folder has no `SKILL.md`"
    );
}

#[tokio::test]
async fn an_embedded_folder_with_a_secret_or_clutter_fails_its_dispatch() {
    static SECRET: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/with-secret");
    static CLUTTER: Skill =
        keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/with-clutter");
    for (skill, path) in [(&SECRET, "secrets.txt"), (&CLUTTER, "node_modules")] {
        let reason = mount_failure(skill).await;
        assert!(
            reason.contains(&format!("`{path}` would be embedded into the binary")),
            "{reason}"
        );
    }
}

#[test]
fn a_code_built_skill_reads_its_name_and_description() {
    let skill = Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap();
    assert_eq!(skill.name(), "haiku");
    assert_eq!(skill.description(), "Write a haiku.");
}

#[test]
fn a_code_built_skill_needs_a_name_and_a_description() {
    let error = |name, description| {
        Skill::new(name, description, "Body.")
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        error(" ", "Write a haiku."),
        "skill ` `: `name` must not be empty"
    );
    assert_eq!(
        error("haiku", ""),
        "skill `haiku`: `description` must not be empty"
    );
}

#[test]
fn a_code_built_skill_file_stays_inside_its_folder() {
    let skill = || Skill::new("haiku", "Write a haiku.", "Body.").unwrap();
    for path in ["/etc/passwd", "../secret.md", "references/../../x", ""] {
        assert!(skill().file(path, "x").is_err(), "{path:?} was accepted");
    }
    assert!(skill().file("references/forms.md", "x").is_ok());
}
