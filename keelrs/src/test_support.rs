//! Helpers that more than one module's tests share. A module's own helpers stay in its tests.

#![allow(clippy::disallowed_methods)] // the folders are set up on the host

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::Skill;

/// A fresh temp folder, removed on drop.
pub(crate) struct Folder(pub(crate) PathBuf);

impl Folder {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-test-{}-{n}", std::process::id()));
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

    /// The path of `relative` in the folder.
    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// What the folder holds at `relative`; `None` when nothing is there.
    pub(crate) fn read(&self, relative: &str) -> Option<String> {
        fs::read_to_string(self.0.join(relative)).ok()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn haiku() -> Skill {
    Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap()
}

pub(crate) fn sonnet() -> Skill {
    Skill::new("sonnet", "Write a sonnet.", "Fourteen lines.")
        .unwrap()
        .file("references/forms.md", "Petrarchan or Shakespearean.")
        .unwrap()
}
