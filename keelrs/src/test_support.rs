//! Helpers that more than one module's tests share. A module's own helpers stay in its tests.

use crate::Skill;

pub(crate) fn haiku() -> Skill {
    Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap()
}

pub(crate) fn sonnet() -> Skill {
    Skill::new("sonnet", "Write a sonnet.", "Fourteen lines.")
        .unwrap()
        .file("references/forms.md", "Petrarchan or Shakespearean.")
        .unwrap()
}
