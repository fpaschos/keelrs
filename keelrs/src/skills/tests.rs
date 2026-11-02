//! The skill tools, each call run as a turn runs it (`agent::rig::call`):
//! no model. How they are declared and listed is tested with the tool loop.

#![allow(clippy::disallowed_methods)] // skill folders are set up on the host

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use super::tools::skill_tools;
use super::{Discover, Skill, discover};
use crate::agent::rig::call;

pub(crate) fn haiku() -> Skill {
    Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap()
}

pub(crate) fn sonnet() -> Skill {
    Skill::new("sonnet", "Write a sonnet.", "Fourteen lines.")
        .unwrap()
        .file("references/forms.md", "Petrarchan or Shakespearean.")
        .unwrap()
}

#[tokio::test]
async fn activating_a_skill_returns_its_instructions_and_lists_its_resources() {
    let text = activate(&[haiku(), sonnet()], "sonnet").await;
    assert_eq!(
        text,
        "Run the skill named \"sonnet\".\n\n\
         <skill_instructions>\nFourteen lines.\n</skill_instructions>\n\n\
         Supporting skill resources are available but are not loaded into context unless needed:\n\
         <skill_resources>\n- references/forms.md\n</skill_resources>"
    );
}

#[tokio::test]
async fn activating_a_skill_without_files_lists_no_resources() {
    let text = activate(&[haiku()], "haiku").await;
    assert_eq!(
        text,
        "Run the skill named \"haiku\".\n\n\
         <skill_instructions>\nThree lines: 5, 7, 5.\n</skill_instructions>"
    );
}

#[tokio::test]
async fn activating_an_unknown_skill_names_the_available_ones() {
    let text = activate(&[haiku(), sonnet()], "limerick").await;
    assert_eq!(
        text,
        "skill \"limerick\" is not available; available skills: haiku, sonnet"
    );
}

/// A skill with one file at `references/file` holding `bytes`.
fn with_file(bytes: impl Into<Vec<u8>>) -> Skill {
    haiku().file("references/file", bytes).unwrap()
}

/// The text the skill tool `name`, built over `skills`, settles to for `arguments`.
async fn called(skills: &[Skill], name: &str, arguments: Value) -> String {
    let tools = skill_tools(skills);
    let tool = tools.iter().find(|tool| &*tool.name == name).unwrap();
    match call(tool, arguments, "c1".to_owned(), None).await {
        Ok(Value::String(text)) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

/// What `activate_skill` returns for the skill `name` among `skills`.
async fn activate(skills: &[Skill], name: &str) -> String {
    called(skills, "activate_skill", json!({"name": name})).await
}

/// What `read_skill_resource` returns for `arguments`, with `skill` mounted.
async fn read(skill: Skill, arguments: Value) -> String {
    called(&[skill], "read_skill_resource", arguments).await
}

#[tokio::test]
async fn a_short_resource_is_read_whole() {
    let text = read(
        sonnet(),
        json!({"skill": "sonnet", "path": "references/forms.md"}),
    )
    .await;
    assert_eq!(text, "Petrarchan or Shakespearean.");
}

static REVIEW: Skill = crate::skill!("$CARGO_MANIFEST_DIR/tests/fixtures/skills/review");

#[tokio::test]
async fn an_embedded_skill_lists_and_reads_the_files_in_its_folder() {
    let text = activate(std::slice::from_ref(&REVIEW), "review").await;
    assert!(
        text.ends_with("<skill_resources>\n- references/style.md\n</skill_resources>"),
        "{text}"
    );
    let text = read(
        REVIEW.clone(),
        json!({"skill": "review", "path": "references/style.md"}),
    )
    .await;
    assert_eq!(text, "# Style\n\nShort sentences.");
}

#[tokio::test]
async fn a_resource_is_paged_by_offset_and_limit() {
    let skill = with_file("one\ntwo\nthree\nfour\nfive\n");
    let arguments = json!({"skill": "haiku", "path": "references/file", "offset": 2, "limit": 2});
    assert_eq!(
        read(skill, arguments).await,
        "two\nthree\n\n[Showing lines 2-3 of 5. Use offset=4 to continue.]"
    );
}

#[tokio::test]
async fn a_long_resource_stops_at_2000_lines() {
    let lines: Vec<String> = (1..=2001).map(|n| n.to_string()).collect();
    let skill = with_file(lines.join("\n"));
    let text = read(skill, json!({"skill": "haiku", "path": "references/file"})).await;
    assert!(text.starts_with("1\n2\n"), "{text}");
    assert!(
        text.ends_with("2000\n\n[Showing lines 1-2000 of 2001. Use offset=2001 to continue.]"),
        "{text}"
    );
}

#[tokio::test]
async fn a_wide_resource_stops_at_50_kb() {
    // 100 lines of 1023 characters plus a newline: 50 fit in 51 200 bytes.
    let line = "x".repeat(1023);
    let skill = with_file(vec![line.as_str(); 100].join("\n"));
    let text = read(skill, json!({"skill": "haiku", "path": "references/file"})).await;
    assert!(
        text.ends_with("\n\n[Showing lines 1-50 of 100. Use offset=51 to continue.]"),
        "{}",
        &text[text.len() - 80..]
    );
}

#[tokio::test]
async fn a_binary_resource_is_base64_wrapped_at_76_columns() {
    let text = read(
        with_file([0xff; 60]),
        json!({"skill": "haiku", "path": "references/file"}),
    )
    .await;
    assert_eq!(text, format!("{}\n////", "/".repeat(76)));
}

#[tokio::test]
async fn a_path_outside_the_skill_or_not_in_it_is_refused() {
    for (path, refusal) in [
        (
            "/references/forms.md",
            "must be relative to the skill's folder",
        ),
        (
            "../haiku/SKILL.md",
            "must be relative to the skill's folder",
        ),
        (
            "references/missing.md",
            "is not a file of skill \"sonnet\"; its files: references/forms.md",
        ),
    ] {
        let text = read(sonnet(), json!({"skill": "sonnet", "path": path})).await;
        assert!(text.contains(refusal), "{path}: {text}");
    }
    let text = read(sonnet(), json!({"skill": "limerick", "path": "x"})).await;
    assert_eq!(
        text,
        "skill \"limerick\" is not available; available skills: sonnet"
    );
}

// Discovered skills: files listed at discovery, read from disk on request.

/// A temp folder of skill folders, removed on drop.
pub(crate) struct Folder(pub(crate) PathBuf);

impl Folder {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-skills-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// Writes `contents` at `relative`, creating its folders.
    pub(crate) fn write(&self, relative: &str, contents: &str) -> &Self {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    /// The one skill discovered in this folder.
    async fn skill(&self) -> Skill {
        let discovered = discover(Discover::new().root(&self.0)).await;
        let [skill] = <[Skill; 1]>::try_from(discovered.skills).unwrap();
        skill
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A folder holding the skill `lint` with two supporting files.
fn lint() -> Folder {
    let folder = Folder::new();
    folder
        .write(
            "lint/SKILL.md",
            "---\nname: lint\ndescription: Lint code.\n---\nRun the linter.\n",
        )
        .write("lint/references/rules.md", "No unused imports.")
        .write("lint/scripts/run.sh", "cargo clippy");
    folder
}

#[tokio::test]
async fn a_discovered_skill_lists_the_files_in_its_folder() {
    let folder = lint();
    let skill = folder.skill().await;
    let text = activate(&[skill], "lint").await;
    assert!(
        text.ends_with(
            "<skill_resources>\n- references/rules.md\n- scripts/run.sh\n</skill_resources>"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn a_discovered_skill_reads_a_file_from_disk_on_request() {
    let folder = lint();
    let skill = folder.skill().await;
    folder.write("lint/references/rules.md", "No unused imports. Edited.");
    let text = read(
        skill,
        json!({"skill": "lint", "path": "references/rules.md"}),
    )
    .await;
    assert_eq!(text, "No unused imports. Edited.");
}

#[cfg(unix)]
#[tokio::test]
async fn a_discovered_skill_refuses_a_symlink_that_leaves_its_folder() {
    let folder = lint();
    let skill = folder.skill().await;
    folder.write("outside.md", "Not the skill's.");
    let rules = folder.0.join("lint/references/rules.md");
    fs::remove_file(&rules).unwrap();
    std::os::unix::fs::symlink(folder.0.join("outside.md"), &rules).unwrap();
    let text = read(
        skill,
        json!({"skill": "lint", "path": "references/rules.md"}),
    )
    .await;
    assert_eq!(text, "`references/rules.md` leaves the skill's folder");
}

#[tokio::test]
async fn a_discovered_skill_never_reads_a_secret() {
    let folder = lint();
    folder.write("lint/.env", "TOKEN=1");
    let skill = folder.skill().await;
    let text = read(skill, json!({"skill": "lint", "path": ".env"})).await;
    assert_eq!(
        text,
        "`.env` is not a file of skill \"lint\"; its files: references/rules.md, scripts/run.sh"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_discovered_skill_lists_a_symlink_that_stays_in_its_folder() {
    let folder = lint();
    std::os::unix::fs::symlink(folder.0.join("lint/references"), folder.0.join("lint/docs"))
        .unwrap();
    let skill = folder.skill().await;
    let text = activate(&[skill], "lint").await;
    assert!(
        text.ends_with(
            "<skill_resources>\n- docs/rules.md\n- references/rules.md\n- scripts/run.sh\n</skill_resources>"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn a_single_line_wider_than_50_kb_is_cut() {
    let skill = with_file("x".repeat(60 * 1024));
    let text = read(skill, json!({"skill": "haiku", "path": "references/file"})).await;
    assert_eq!(
        text,
        format!(
            "{}\n\n[Line 1 is cut at 51200 of 61440 bytes.]",
            "x".repeat(50 * 1024)
        )
    );
}

#[tokio::test]
async fn a_file_path_with_dot_segments_is_stored_and_read_normalized() {
    let skill = haiku().file("./refs//a/./b.md", "B").unwrap();
    let text = activate(std::slice::from_ref(&skill), "haiku").await;
    assert!(
        text.ends_with("<skill_resources>\n- refs/a/b.md\n</skill_resources>"),
        "{text}"
    );
    for path in ["refs/a/b.md", "./refs/a/b.md", "refs//a/./b.md"] {
        let text = read(skill.clone(), json!({"skill": "haiku", "path": path})).await;
        assert_eq!(text, "B", "{path}");
    }
}
