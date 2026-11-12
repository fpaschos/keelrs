//! Skills: building them, the catalog, and the built-in `activate_skill`
//! and `read_skill_resource`.

use keelrs::{Agent, AgentContext, Skill};

use crate::support::{TempDir, agent, clock, first_request, haiku, open, sonnet, tool_names};

#[test]
fn an_embedded_skill_may_hold_templates_named_like_secrets() {
    let skill: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/with-templates");
    assert_eq!(skill.description(), "Set up a project's environment.");
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

#[tokio::test]
async fn the_catalog_follows_the_instructions_in_use_skill_order() {
    let sb = open(&TempDir::new()).await;
    let agent = Agent::new("poet", move |cx: &mut AgentContext| {
        cx.use_model("scripted/model");
        cx.use_sandbox(&sb).only([]);
        cx.use_skills([sonnet(), haiku()]);
        "Write poems.".into()
    });
    assert_eq!(
        first_request(&agent).await.instructions.as_deref(),
        Some(
            "Write poems.\n\n\
             ## Available Skills\n\n\
             When a task matches a skill description, call the `activate_skill` tool with \
             that skill name before proceeding so its full instructions are loaded.\n\n\
             - **sonnet** — Write a sonnet.\n\
             - **haiku** — Write a haiku."
        )
    );
}

#[tokio::test]
async fn activate_skill_is_declared_with_skills_and_read_skill_resource_with_files() {
    let without_files = agent(|cx| cx.use_skill(haiku())).await;
    let request = first_request(&without_files).await;
    assert_eq!(tool_names(&request), ["activate_skill"]);

    let with_files = agent(|cx| {
        cx.use_tool(clock());
        cx.use_skills([haiku(), sonnet()]);
    })
    .await;
    let request = first_request(&with_files).await;
    assert_eq!(
        tool_names(&request),
        ["activate_skill", "read_skill_resource", "current_time"]
    );
}

#[tokio::test]
async fn a_multi_line_description_is_one_catalog_line() {
    let agent = agent(|cx| {
        cx.use_skill(Skill::new("haiku", "Write a haiku.\nShort ones.\n", "Body.").unwrap());
    })
    .await;
    let instructions = first_request(&agent).await.instructions.unwrap();
    assert!(
        instructions.ends_with("\n\n- **haiku** — Write a haiku. Short ones."),
        "{instructions:?}"
    );
}
