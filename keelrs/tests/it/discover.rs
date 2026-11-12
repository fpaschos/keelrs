//! Discovering skill folders on disk, through the public API, over temp-dir fixtures.

use keelrs::Skill;
use keelrs::skills::{Discover, SkillProblem, discover};

use crate::support::{skill, temp_dir, write};

/// `(name, description)` of each discovered skill, in order.
fn found(discovered: &keelrs::skills::Discovered) -> Vec<(&str, &str)> {
    discovered
        .skills
        .iter()
        .map(|skill| (skill.name(), skill.description()))
        .collect()
}

#[tokio::test]
async fn a_root_yields_its_skill_folders_in_name_order() {
    let fixture = temp_dir();
    skill(&fixture, "skills", "review", "review", "Review code.");
    skill(&fixture, "skills", "haiku", "haiku", "Write a haiku.");
    let discovered = discover(Discover::new().root(fixture.join("skills"))).await;
    assert_eq!(
        found(&discovered),
        [("haiku", "Write a haiku."), ("review", "Review code.")]
    );
    assert_eq!(discovered.problems, []);
}

#[tokio::test]
async fn a_folder_that_cannot_be_a_skill_is_skipped_with_a_problem() {
    let fixture = temp_dir();
    skill(&fixture, "skills", "haiku", "haiku", "Write a haiku.");
    write(
        &fixture,
        "skills/bare/SKILL.md",
        "---\nname: bare\n---\nBody.\n",
    );
    write(
        &fixture,
        "skills/broken/SKILL.md",
        "---\nname: [broken\n---\nBody.\n",
    );
    write(&fixture, "skills/notes/todo.md", "Not a skill.");
    write(&fixture, "skills/README.md", "Not a folder.");
    let discovered = discover(Discover::new().root(fixture.join("skills"))).await;
    assert_eq!(found(&discovered), [("haiku", "Write a haiku.")]);
    let problems: Vec<_> = discovered.problems.iter().map(|p| &p.path).collect();
    assert_eq!(
        problems,
        [&fixture.join("skills/bare"), &fixture.join("skills/broken")]
    );
    assert!(
        discovered.problems[0].reason.contains("description"),
        "{:?}",
        discovered.problems[0]
    );
    assert!(
        discovered.problems[1].reason.starts_with("frontmatter:"),
        "{:?}",
        discovered.problems[1]
    );
}

#[tokio::test]
async fn an_unquoted_value_with_a_colon_is_read_as_text() {
    let fixture = temp_dir();
    write(
        &fixture,
        "skills/review/SKILL.md",
        "---\nname: review\ndescription: Use when: the user asks for a review: code or docs\n\
             argument-hint: [path]\n---\nBody.\n",
    );
    let discovered = discover(Discover::new().root(fixture.join("skills"))).await;
    assert_eq!(
        found(&discovered),
        [(
            "review",
            "Use when: the user asks for a review: code or docs"
        )]
    );
    assert_eq!(discovered.problems, []);
}

#[tokio::test]
async fn a_secret_file_is_left_out_with_a_problem_and_clutter_silently() {
    let fixture = temp_dir();
    skill(&fixture, "skills", "deploy", "deploy", "Deploy the app.");
    write(&fixture, "skills/deploy/.env.local", "TOKEN=1");
    write(&fixture, "skills/deploy/keys/server.pem", "-----BEGIN");
    write(&fixture, "skills/deploy/node_modules/left-pad/index.js", "");
    write(&fixture, "skills/deploy/.git/HEAD", "ref: main");
    write(&fixture, "skills/deploy/templates/.env.example", "API_KEY=");
    write(
        &fixture,
        "skills/deploy/references/secrets-handling.md",
        "Use the env.",
    );
    let discovered = discover(Discover::new().root(fixture.join("skills"))).await;
    assert_eq!(found(&discovered), [("deploy", "Deploy the app.")]);
    let reason = "may hold a secret; left out of the skill's files".to_owned();
    assert_eq!(
        discovered.problems,
        [
            SkillProblem {
                path: fixture.join("skills/deploy/.env.local"),
                reason: reason.clone(),
            },
            SkillProblem {
                path: fixture.join("skills/deploy/keys/server.pem"),
                reason,
            },
        ]
    );
}

#[tokio::test]
async fn conventional_roots_read_agents_before_claude() {
    let fixture = temp_dir();
    skill(
        &fixture,
        ".agents/skills",
        "a",
        "keelrs-test-review",
        "From .agents.",
    );
    skill(
        &fixture,
        ".claude/skills",
        "b",
        "keelrs-test-review",
        "From .claude.",
    );
    skill(
        &fixture,
        ".claude/skills",
        "c",
        "keelrs-test-haiku",
        "Write a haiku.",
    );
    let discovered = discover(Discover::conventional(fixture.join(""))).await;
    let ours: Vec<_> = found(&discovered)
        .into_iter()
        .filter(|(name, _)| name.starts_with("keelrs-test-"))
        .collect();
    assert_eq!(
        ours,
        [
            ("keelrs-test-review", "From .agents."),
            ("keelrs-test-haiku", "Write a haiku.")
        ]
    );
    assert!(
        discovered
            .problems
            .iter()
            .any(|p| p.path == fixture.join(".claude/skills/b"))
    );
}

#[tokio::test]
async fn a_discovered_skill_is_found_by_name() {
    let fixture = temp_dir();
    skill(&fixture, "skills", "haiku", "haiku", "Write a haiku.");
    let discovered = discover(Discover::new().root(fixture.join("skills"))).await;
    assert_eq!(
        discovered.get("haiku").map(Skill::description),
        Some("Write a haiku.")
    );
    assert!(discovered.get("sonnet").is_none());
    let names: Vec<&str> = (&discovered).into_iter().map(Skill::name).collect();
    assert_eq!(names, ["haiku"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_that_leaves_the_skill_folder_is_left_out_with_a_problem() {
    let fixture = temp_dir();
    skill(&fixture, "skills", "lint", "lint", "Lint code.");
    write(&fixture, "shared/rules.md", "Shared.");
    std::os::unix::fs::symlink(fixture.join("shared"), fixture.join("skills/lint/shared")).unwrap();
    std::os::unix::fs::symlink(
        fixture.join("skills/lint"),
        fixture.join("skills/lint/loop"),
    )
    .unwrap();
    let discovered = discover(Discover::new().root(fixture.join("skills"))).await;
    assert_eq!(found(&discovered), [("lint", "Lint code.")]);
    assert_eq!(
        discovered.problems,
        [SkillProblem {
            path: fixture.join("skills/lint/shared"),
            reason: "a symlink that leaves the skill's folder; left out of the skill's files"
                .to_owned(),
        }]
    );
}

#[tokio::test]
async fn the_first_root_wins_a_name_and_the_later_folder_is_a_problem() {
    let fixture = temp_dir();
    skill(
        &fixture,
        "project",
        "review",
        "review",
        "Review this project's code.",
    );
    skill(&fixture, "user", "review", "review", "Review any code.");
    skill(&fixture, "user", "haiku", "haiku", "Write a haiku.");
    let discovered = discover(
        Discover::new()
            .root(fixture.join("missing"))
            .root(fixture.join("project"))
            .root(fixture.join("user")),
    )
    .await;
    assert_eq!(
        found(&discovered),
        [
            ("review", "Review this project's code."),
            ("haiku", "Write a haiku.")
        ]
    );
    assert_eq!(
        discovered.problems,
        [SkillProblem {
            path: fixture.join("user/review"),
            reason: format!(
                "skill `review` is already discovered at `{}`",
                fixture.join("project/review").display()
            ),
        }]
    );
}
