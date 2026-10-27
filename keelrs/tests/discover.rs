//! Discovering skill folders on disk, through the public API, over temp-dir fixtures.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{fs, process};

use keelrs::Skill;
use keelrs::skills::{Discover, SkillProblem, discover};

/// A temp folder removed on drop.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-discover-{}-{n}", process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// The fixture path `relative`.
    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// Writes `contents` at `relative`, creating its folders.
    fn write(&self, relative: &str, contents: &str) -> &Self {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    /// Writes `<root>/<folder>/SKILL.md` naming the skill `name`.
    fn skill(&self, root: &str, folder: &str, name: &str, description: &str) -> &Self {
        self.write(
            &format!("{root}/{folder}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nBody of {name}.\n"),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
    let fixture = Fixture::new();
    fixture
        .skill("skills", "review", "review", "Review code.")
        .skill("skills", "haiku", "haiku", "Write a haiku.");
    let discovered = discover(Discover::new().root(fixture.path("skills"))).await;
    assert_eq!(
        found(&discovered),
        [("haiku", "Write a haiku."), ("review", "Review code.")]
    );
    assert_eq!(discovered.problems, []);
}

#[tokio::test]
async fn a_folder_that_cannot_be_a_skill_is_skipped_with_a_problem() {
    let fixture = Fixture::new();
    fixture
        .skill("skills", "haiku", "haiku", "Write a haiku.")
        .write("skills/bare/SKILL.md", "---\nname: bare\n---\nBody.\n")
        .write("skills/broken/SKILL.md", "---\nname: [broken\n---\nBody.\n")
        .write("skills/notes/todo.md", "Not a skill.")
        .write("skills/README.md", "Not a folder.");
    let discovered = discover(Discover::new().root(fixture.path("skills"))).await;
    assert_eq!(found(&discovered), [("haiku", "Write a haiku.")]);
    let problems: Vec<_> = discovered.problems.iter().map(|p| &p.path).collect();
    assert_eq!(
        problems,
        [&fixture.path("skills/bare"), &fixture.path("skills/broken")]
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
    let fixture = Fixture::new();
    fixture.write(
        "skills/review/SKILL.md",
        "---\nname: review\ndescription: Use when: the user asks for a review: code or docs\n\
         argument-hint: [path]\n---\nBody.\n",
    );
    let discovered = discover(Discover::new().root(fixture.path("skills"))).await;
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
    let fixture = Fixture::new();
    fixture
        .skill("skills", "deploy", "deploy", "Deploy the app.")
        .write("skills/deploy/.env.local", "TOKEN=1")
        .write("skills/deploy/keys/server.pem", "-----BEGIN")
        .write("skills/deploy/node_modules/left-pad/index.js", "")
        .write("skills/deploy/.git/HEAD", "ref: main");
    let discovered = discover(Discover::new().root(fixture.path("skills"))).await;
    assert_eq!(found(&discovered), [("deploy", "Deploy the app.")]);
    let reason = "may hold a secret; left out of the skill's files".to_owned();
    assert_eq!(
        discovered.problems,
        [
            SkillProblem {
                path: fixture.path("skills/deploy/.env.local"),
                reason: reason.clone(),
            },
            SkillProblem {
                path: fixture.path("skills/deploy/keys/server.pem"),
                reason,
            },
        ]
    );
}

#[tokio::test]
async fn conventional_roots_read_agents_before_claude() {
    let fixture = Fixture::new();
    fixture
        .skill(".agents/skills", "a", "keelrs-test-review", "From .agents.")
        .skill(".claude/skills", "b", "keelrs-test-review", "From .claude.")
        .skill(".claude/skills", "c", "keelrs-test-haiku", "Write a haiku.");
    let discovered = discover(Discover::conventional(fixture.path(""))).await;
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
            .any(|p| p.path == fixture.path(".claude/skills/b"))
    );
}

#[tokio::test]
async fn a_discovered_skill_is_found_by_name() {
    let fixture = Fixture::new();
    fixture.skill("skills", "haiku", "haiku", "Write a haiku.");
    let discovered = discover(Discover::new().root(fixture.path("skills"))).await;
    assert_eq!(
        discovered.get("haiku").map(Skill::description),
        Some("Write a haiku.")
    );
    assert!(discovered.get("sonnet").is_none());
    let names: Vec<&str> = (&discovered).into_iter().map(Skill::name).collect();
    assert_eq!(names, ["haiku"]);
}

#[tokio::test]
async fn the_first_root_wins_a_name_and_the_later_folder_is_a_problem() {
    let fixture = Fixture::new();
    fixture
        .skill("project", "review", "review", "Review this project's code.")
        .skill("user", "review", "review", "Review any code.")
        .skill("user", "haiku", "haiku", "Write a haiku.");
    let discovered = discover(
        Discover::new()
            .root(fixture.path("missing"))
            .root(fixture.path("project"))
            .root(fixture.path("user")),
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
            path: fixture.path("user/review"),
            reason: format!(
                "skill `review` is already discovered at `{}`",
                fixture.path("project/review").display()
            ),
        }]
    );
}
