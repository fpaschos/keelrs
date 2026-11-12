//! Skills embedded at compile time with [`skill!`](crate::skill!).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use include_dir::{Dir, DirEntry};

use super::{Files, Parsed, SkillError, forbidden, frontmatter};

/// A skill folder embedded by [`skill!`](crate::skill!), parsed on first use.
#[doc(hidden)]
#[derive(Debug)]
pub struct Embedded {
    path: &'static str,
    dir: &'static Dir<'static>,
    parsed: OnceLock<Result<Parsed, SkillError>>,
}

impl Embedded {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(path: &'static str, dir: &'static Dir<'static>) -> Self {
        Self {
            path,
            dir,
            parsed: OnceLock::new(),
        }
    }

    pub(super) fn parsed(&self) -> &Result<Parsed, SkillError> {
        self.parsed.get_or_init(|| {
            let folder = self.path.rsplit('/').next().unwrap_or(self.path);
            self.parse()
                .map_err(|reason| SkillError::new(folder, reason))
        })
    }

    fn parse(&self) -> Result<Parsed, String> {
        let mut files = BTreeMap::new();
        collect(self.dir, &mut files)?;
        let source = files
            .remove("SKILL.md")
            .ok_or_else(|| "the folder has no `SKILL.md`".to_owned())?;
        let source = String::from_utf8(source).map_err(|_| "`SKILL.md` is not UTF-8".to_owned())?;
        let (frontmatter, body) = frontmatter::parse(&source)?;
        Ok(Parsed {
            frontmatter,
            instructions: body.to_owned(),
            files: Files::Memory(files),
        })
    }
}

/// Every file under `dir` by its path from the skill's folder, refusing files
/// that must not ship inside a binary.
fn collect(dir: &Dir<'_>, files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    for entry in dir.entries() {
        let path = entry.path().to_string_lossy();
        if path.split('/').any(forbidden) {
            return Err(format!(
                "`{path}` would be embedded into the binary; remove it from the skill's folder"
            ));
        }
        match entry {
            DirEntry::Dir(dir) => collect(dir, files)?,
            DirEntry::File(file) => {
                files.insert(path.into_owned(), file.contents().to_vec());
            }
        }
    }
    Ok(())
}

/// Whether a [`skill!`](crate::skill!) path starts at the crate root.
#[doc(hidden)]
#[must_use]
pub const fn from_manifest_dir(path: &str) -> bool {
    let (path, prefix) = (path.as_bytes(), b"$CARGO_MANIFEST_DIR/");
    if path.len() < prefix.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if path[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Embeds the whole skill folder at `path` as a [`Skill`]: its `SKILL.md` and
/// every other file in it, subfolders included.
///
/// `path` starts at the crate root, as `"$CARGO_MANIFEST_DIR/skills/review"`.
/// A missing folder fails to compile. The rest is checked when an agent first
/// mounts the skill, and a broken skill settles that dispatch as
/// [`Outcome::Failed`](crate::Outcome::Failed): a missing `SKILL.md`, an empty
/// `name` or `description`, or a file that must not ship inside a binary
/// (`.git`, `.ssh`, `node_modules`, `dist`, `target`, `secrets*` but Markdown,
/// `.env*` but `.example`, `.sample` and `.template`, `*.pem`, `*.key`). Symlinks
/// are followed at compile time.
///
/// ```ignore
/// static REVIEW: Skill = keelrs::skill!("$CARGO_MANIFEST_DIR/skills/review");
/// ```
#[macro_export]
macro_rules! skill {
    // `tt`, not `literal`: `include_dir!` rejects a literal wrapped in a macro group.
    ($path:tt) => {{
        use $crate::__private::include_dir;
        const _: () = assert!(
            $crate::skills::from_manifest_dir($path),
            "`skill!` paths start at the crate root: \"$CARGO_MANIFEST_DIR/<folder>\""
        );
        static DIR: include_dir::Dir<'static> = include_dir::include_dir!($path);
        static EMBEDDED: $crate::skills::Embedded = $crate::skills::Embedded::new($path, &DIR);
        $crate::Skill::embedded(&EMBEDDED)
    }};
}
