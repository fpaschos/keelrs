//! Building skills, through the public API.

use keelrs::Skill;

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
