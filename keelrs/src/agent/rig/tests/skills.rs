//! Skills: the catalog and the built-in `activate_skill` and `read_skill_resource`.

use super::*;

#[tokio::test]
async fn the_catalog_follows_the_instructions_in_use_skill_order() {
    let sb = unused();
    let agent = Agent::new("poet", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        cx.use_sandbox(&sb).only([]);
        cx.use_skills([sonnet(), haiku()]);
        "Write poems.".into()
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(
        preamble(&requests[0]),
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
    let without_files = agent(|cx| cx.use_skill(haiku()));
    let (_, requests) = sent(&without_files, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["activate_skill"]);

    let with_files = agent(|cx| {
        cx.use_tool(clock());
        cx.use_skills([haiku(), sonnet()]);
    });
    let (_, requests) = sent(&with_files, [MockTurn::text("Done.")]).await;
    assert_eq!(
        tool_names(&requests[0]),
        ["activate_skill", "read_skill_resource", "current_time"]
    );
}

#[tokio::test]
async fn a_turn_without_skills_or_a_sandbox_declares_only_its_own_tools() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(preamble(&requests[0]), None);
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

#[tokio::test]
async fn a_multi_line_description_is_one_catalog_line() {
    let agent = agent(|cx| {
        cx.use_skill(Skill::new("haiku", "Write a haiku.\nShort ones.\n", "Body.").unwrap());
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert!(
        preamble(&requests[0])
            .unwrap()
            .ends_with("\n\n- **haiku** — Write a haiku. Short ones."),
        "{:?}",
        preamble(&requests[0])
    );
}
