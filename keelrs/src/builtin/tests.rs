//! The built-in tools against `sandbox::local()` on a temp folder: each
//! tool's text and refusals. How they reach a turn is tested with the tool
//! loop, in `agent::rig::tests`.

#![allow(clippy::disallowed_methods)] // the fixture files are set up on the host

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rig_tools::Seen;

use super::bash::{BashArgs, bash, description};
use super::edit::{EditArgs, Replacement, edit};
use super::files::{DeleteArgs, MoveArgs, delete, move_file};
use super::glob::{GlobArgs, glob};
use super::grep::{GrepArgs, grep};
use super::read::{ReadArgs, read};
use super::write::{WriteArgs, write};
use crate::sandbox::{self, Sandbox, SandboxPath};

/// A temp folder with a local sandbox rooted in it, removed on drop.
struct Fixture {
    dir: PathBuf,
    sb: Sandbox,
    seen: Seen,
}

impl Fixture {
    async fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("keelrs-builtin-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let sb = sandbox::local().root(&dir).open().await.unwrap();
        Self {
            dir,
            sb,
            seen: Seen::default(),
        }
    }

    /// Writes `contents` at `relative` on the host, creating its folders.
    fn put(&self, relative: &str, contents: &str) -> &Self {
        let path = self.dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    /// What the host holds at `relative`.
    fn get(&self, relative: &str) -> String {
        fs::read_to_string(self.dir.join(relative)).unwrap()
    }

    async fn read(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> String {
        let args = ReadArgs {
            path: path.to_owned(),
            offset,
            limit,
        };
        shown(read(&self.sb, &self.seen, args).await)
    }

    async fn write(&self, path: &str, content: &str) -> String {
        let args = WriteArgs {
            path: path.to_owned(),
            content: content.to_owned(),
        };
        shown(write(&self.sb, &self.seen, args).await)
    }

    /// One `edit` call of `path`, each edit `(old_text, new_text, replace_all)`.
    async fn edit(&self, path: &str, edits: &[(&str, &str, bool)]) -> String {
        let edits = edits
            .iter()
            .map(|&(old, new, all)| Replacement {
                old_text: old.to_owned(),
                new_text: new.to_owned(),
                replace_all: all,
            })
            .collect();
        let args = EditArgs {
            path: path.to_owned(),
            edits,
        };
        shown(edit(&self.sb, &self.seen, args).await)
    }

    async fn delete(&self, path: &str) -> String {
        let args = DeleteArgs {
            path: path.to_owned(),
        };
        shown(delete(&self.sb, &self.seen, args).await)
    }

    async fn move_file(&self, from: &str, to: &str, overwrite: bool) -> String {
        let args = MoveArgs {
            from: from.to_owned(),
            to: to.to_owned(),
            overwrite,
        };
        shown(move_file(&self.sb, &self.seen, args).await)
    }

    fn exists(&self, relative: &str) -> bool {
        self.dir.join(relative).exists()
    }

    async fn glob(&self, pattern: &str, path: Option<&str>, hidden: bool) -> String {
        let args = GlobArgs {
            pattern: pattern.to_owned(),
            path: path.map(str::to_owned),
            hidden,
        };
        shown(glob(&self.sb, args).await)
    }

    async fn grep(&self, pattern: &str, more: impl FnOnce(&mut GrepArgs)) -> String {
        let mut args = GrepArgs {
            pattern: pattern.to_owned(),
            path: None,
            glob: None,
            ignore_case: false,
            literal: false,
            context: None,
            hidden: false,
        };
        more(&mut args);
        shown(grep(&self.sb, args).await)
    }

    async fn bash(&self, command: &str, timeout: Option<u64>) -> String {
        let args = BashArgs {
            command: command.to_owned(),
            timeout,
        };
        shown(bash(&self.sb, args).await)
    }

    /// A small project: ignored, hidden and nested files beside the visible ones.
    fn project(&self) -> &Self {
        self.put(".gitignore", "gen/\n")
            .put("README.md", "# fn readme\n")
            .put("docs/guide.md", "fn guide\n")
            .put("src/lib.rs", LIB)
            .put(
                "src/main.rs",
                "fn main() {\n    println!(\"{}\", greet(\"x\"));\n}\n",
            )
            .put("src/deep/x.rs", "fn deep() {}\n")
            .put("gen/out.rs", "fn generated() {}\n")
            .put(".hidden/notes.rs", "fn hidden() {}\n")
    }
}

/// The file the `edit` cases change, with typographic quotes on line 2.
const LIB: &str = "pub fn greet(name: &str) -> String {
    format!(“Hello, {name}”)
}

pub fn twice(x: i32) -> i32 {
    x * 2
}

pub fn thrice(x: i32) -> i32 {
    x * 3
}
";

/// A tool's text, or its refusal as `Err: …`.
fn shown(out: super::Out) -> String {
    out.unwrap_or_else(|e| format!("Err: {e}"))
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// read

#[tokio::test]
async fn read_shows_a_whole_file() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\ntwo\n");
    assert_eq!(fx.read("a.txt", None, None).await, "one\ntwo\n");
}

#[tokio::test]
async fn read_shows_a_range_and_where_to_read_on() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\ntwo\nthree\n");
    assert_eq!(
        fx.read("a.txt", Some(2), Some(1)).await,
        "two\n[1 more lines; read on with offset 3]\n"
    );
}

#[tokio::test]
async fn read_shows_crlf_lines_without_their_cr() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\r\ntwo\r\n");
    assert_eq!(fx.read("a.txt", None, None).await, "one\ntwo\n");
}

#[tokio::test]
async fn read_of_a_missing_file_names_the_path_it_was_given() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.read("src/missing.rs", None, None).await,
        "Err: src/missing.rs: no such file"
    );
}

// write

#[tokio::test]
async fn write_creates_a_file_and_its_folders() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.write("src/new.rs", "fn main() {}\n").await,
        "Created src/new.rs."
    );
    assert_eq!(fx.get("src/new.rs"), "fn main() {}\n");
}

#[tokio::test]
async fn write_replaces_a_file_and_says_how_long_it_was() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\ntwo\n");
    assert_eq!(
        fx.write("a.txt", "three\n").await,
        "Replaced a.txt, which had 2 lines."
    );
    assert_eq!(fx.get("a.txt"), "three\n");
}

// edit

#[tokio::test]
async fn edit_replaces_exact_text_and_shows_the_lines_around_it() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    fx.read("src/lib.rs", None, None).await;
    assert_eq!(
        fx.edit("src/lib.rs", &[("    x * 2", "    x + x", false)])
            .await,
        "Edited src/lib.rs: line 6.\n[lines 3-9]\n}\n\npub fn twice(x: i32) -> i32 {\n    x + x\n}\n\npub fn thrice(x: i32) -> i32 {"
    );
    assert_eq!(fx.get("src/lib.rs"), LIB.replace("x * 2", "x + x"));
}

#[tokio::test]
async fn edit_matches_loosely_and_says_so() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    let edited = fx
        .edit(
            "src/lib.rs",
            &[(
                "    format!(\"Hello, {name}\")",
                "    format!(\"Hi, {name}\")",
                false,
            )],
        )
        .await;
    assert_eq!(
        edited,
        "Edited src/lib.rs: line 2.\nNote: `old_text` matched only after evening out trailing whitespace and quote, dash or space characters; copy text exactly next time\n[lines 1-5]\npub fn greet(name: &str) -> String {\n    format!(\"Hi, {name}\")\n}\n\npub fn twice(x: i32) -> i32 {"
    );
}

#[tokio::test]
async fn edit_refuses_an_old_text_that_matches_several_places() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.edit("src/lib.rs", &[("pub fn", "fn", false)]).await,
        "Err: `old_text` matches 3 places, starting at lines 1, 5, 9; add the lines around the one to change so it matches once, or set `replace_all` to change every one. `src/lib.rs` was not changed; fix the edit and send it again."
    );
    assert_eq!(fx.get("src/lib.rs"), LIB);
}

#[tokio::test]
async fn edit_refuses_an_old_text_found_nowhere() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.edit("src/lib.rs", &[("zzz qqq", "x", false)]).await,
        "Err: `old_text` was not found, not even ignoring trailing whitespace, and nothing in the file is close to it; check the path and the text. `src/lib.rs` was not changed; fix the edit and send it again."
    );
}

#[tokio::test]
async fn edit_with_replace_all_changes_every_match() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.edit("src/lib.rs", &[("i32", "i64", true)]).await,
        "Edited src/lib.rs: 4 replacements, lines 5, 9.\n[lines 2-11]\n    format!(“Hello, {name}”)\n}\n\npub fn twice(x: i64) -> i64 {\n    x * 2\n}\n\npub fn thrice(x: i64) -> i64 {\n    x * 3\n}"
    );
    assert_eq!(fx.get("src/lib.rs"), LIB.replace("i32", "i64"));
}

#[tokio::test]
async fn edit_keeps_crlf_line_endings() {
    let fx = Fixture::new().await;
    fx.put("crlf.txt", "a\r\nb\r\nc\r\n");
    assert_eq!(
        fx.edit("crlf.txt", &[("b", "B", false)]).await,
        "Edited crlf.txt: line 2.\n[lines 1-3]\na\nB\nc"
    );
    assert_eq!(fx.get("crlf.txt"), "a\r\nB\r\nc\r\n");
}

#[tokio::test]
async fn edit_of_a_file_changed_elsewhere_since_it_was_read_keeps_that_change() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    fx.read("src/lib.rs", None, None).await;
    fx.put("src/lib.rs", &format!("{LIB}// someone else\n"));
    assert_eq!(
        fx.edit("src/lib.rs", &[("    x * 3", "    x + x + x", false)])
            .await,
        "Edited src/lib.rs: line 10.\n[lines 7-12]\n}\n\npub fn thrice(x: i32) -> i32 {\n    x + x + x\n}\n// someone else"
    );
    assert_eq!(
        fx.get("src/lib.rs"),
        format!("{}// someone else\n", LIB.replace("x * 3", "x + x + x"))
    );
}

#[tokio::test]
async fn edit_of_text_changed_since_it_was_read_asks_for_a_new_read() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    fx.read("src/lib.rs", None, None).await;
    fx.put("src/lib.rs", &LIB.replace("x * 3", "3 * x"));
    assert_eq!(
        fx.edit("src/lib.rs", &[("    x * 3", "    x + x + x", false)])
            .await,
        "Err: `old_text` was not found, not even ignoring trailing whitespace: `src/lib.rs` changed since it was last read, so read it again and copy `old_text` from what it holds now. `src/lib.rs` was not changed; fix the edit and send it again."
    );
}

// delete and move

/// What an `edit` of text not in `path` says when `path` counts as read.
const STALE: &str = "Err: `old_text` was not found, not even ignoring trailing whitespace: `b.txt` changed since it was last read, so read it again and copy `old_text` from what it holds now. `b.txt` was not changed; fix the edit and send it again.";

#[tokio::test]
async fn delete_removes_a_file() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n");
    assert_eq!(fx.delete("a.txt").await, "Deleted a.txt.");
    assert!(!fx.exists("a.txt"));
}

#[tokio::test]
async fn delete_refuses_a_directory_and_a_missing_file() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.delete("src").await,
        "Err: src is not a regular file; `delete` removes files only"
    );
    assert_eq!(fx.delete("nope.txt").await, "Err: nope.txt: no such file");
    assert!(fx.exists("src/lib.rs"));
}

#[tokio::test]
async fn delete_forgets_what_was_read() {
    let fx = Fixture::new().await;
    fx.put("b.txt", "one\n");
    fx.read("b.txt", None, None).await;
    fx.delete("b.txt").await;
    fx.put("b.txt", "two\n");
    assert_ne!(fx.edit("b.txt", &[("one", "1", false)]).await, STALE);
}

#[tokio::test]
async fn move_renames_a_file_creating_the_new_folders() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n");
    assert_eq!(
        fx.move_file("a.txt", "new/b.txt", false).await,
        "Moved a.txt to new/b.txt."
    );
    assert!(!fx.exists("a.txt"));
    assert_eq!(fx.get("new/b.txt"), "one\n");
}

#[tokio::test]
async fn move_refuses_a_directory() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.move_file("src", "lib", false).await,
        "Err: src is not a regular file; `move` moves files only"
    );
}

#[tokio::test]
async fn move_refuses_an_existing_target_unless_told_to_overwrite() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n").put("b.txt", "two\n");
    assert_eq!(
        fx.move_file("a.txt", "b.txt", false).await,
        "Err: b.txt already exists; set `overwrite` to replace it"
    );
    assert_eq!(fx.get("b.txt"), "two\n");
    assert_eq!(
        fx.move_file("a.txt", "b.txt", true).await,
        "Moved a.txt to b.txt."
    );
    assert_eq!(fx.get("b.txt"), "one\n");
}

#[tokio::test]
async fn move_carries_what_was_read_to_the_new_path() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n");
    fx.read("a.txt", None, None).await;
    fx.move_file("a.txt", "b.txt", false).await;
    fx.put("b.txt", "two\n");
    assert_eq!(fx.edit("b.txt", &[("one", "1", false)]).await, STALE);
}

// glob

#[tokio::test]
async fn glob_lists_matching_files_sorted_leaving_out_ignored_and_hidden_ones() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.glob("**/*.rs", None, false).await,
        "src/deep/x.rs\nsrc/lib.rs\nsrc/main.rs"
    );
}

#[tokio::test]
async fn glob_is_a_path_glob_from_the_searched_directory() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(fx.glob("*.md", None, false).await, "README.md");
    assert_eq!(
        fx.glob("src/*", None, false).await,
        "src/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        fx.glob("*.rs", Some("src"), false).await,
        "src/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        fx.glob("./src/*", None, false).await,
        "src/lib.rs\nsrc/main.rs"
    );
}

#[tokio::test]
async fn glob_with_hidden_lists_hidden_files_but_never_ignored_ones() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.glob("**/*.rs", None, true).await,
        ".hidden/notes.rs\nsrc/deep/x.rs\nsrc/lib.rs\nsrc/main.rs"
    );
}

#[tokio::test]
async fn glob_says_when_nothing_matches() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(fx.glob("**/*.py", None, false).await, "No files found.");
}

#[tokio::test]
async fn glob_refuses_a_bad_glob_saying_why() {
    let fx = Fixture::new().await;
    fx.project();
    let bad = fx.glob("src/[", None, false).await;
    assert!(bad.starts_with("Err: bad glob: "), "{bad}");
}

#[tokio::test]
async fn glob_stops_at_1000_paths() {
    let fx = Fixture::new().await;
    for n in 0..1001 {
        fx.put(&format!("many/{n:04}.txt"), "");
    }
    let listed = fx.glob("many/*", None, false).await;
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 1001);
    assert_eq!(lines[999], "many/0999.txt");
    assert_eq!(
        lines[1000],
        "[showing 1000 of 1001 paths; narrow the pattern or path]"
    );
}

// grep

#[tokio::test]
async fn grep_lists_matching_lines_leaving_out_ignored_and_hidden_files() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep("fn ", |_| {}).await,
        "README.md:1: # fn readme
docs/guide.md:1: fn guide
src/deep/x.rs:1: fn deep() {}
src/lib.rs:1: pub fn greet(name: &str) -> String {
src/lib.rs:5: pub fn twice(x: i32) -> i32 {
src/lib.rs:9: pub fn thrice(x: i32) -> i32 {
src/main.rs:1: fn main() {"
    );
}

#[tokio::test]
async fn grep_with_hidden_searches_hidden_files_but_never_ignored_ones() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep("fn (hidden|generated)", |a| a.hidden = true).await,
        ".hidden/notes.rs:1: fn hidden() {}"
    );
}

#[tokio::test]
async fn grep_searches_a_path_or_one_file_and_only_files_matching_a_glob() {
    let fx = Fixture::new().await;
    fx.project();
    let in_src = fx
        .grep(r"x \* \d", |a| {
            a.path = Some("src".to_owned());
            a.glob = Some("*.rs".to_owned());
        })
        .await;
    assert_eq!(in_src, "src/lib.rs:6:     x * 2\nsrc/lib.rs:10:     x * 3");
    let one_file = fx
        .grep("thrice", |a| a.path = Some("src/lib.rs".to_owned()))
        .await;
    assert_eq!(one_file, "src/lib.rs:9: pub fn thrice(x: i32) -> i32 {");
}

#[tokio::test]
async fn grep_matches_literal_text_and_ignores_case_when_asked() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep("x * 2", |a| a.literal = true).await,
        "src/lib.rs:6:     x * 2"
    );
    assert_eq!(
        fx.grep("FN MAIN", |a| a.ignore_case = true).await,
        "src/main.rs:1: fn main() {"
    );
}

#[tokio::test]
async fn grep_shows_context_lines_and_separates_groups() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep(r"x \* [23]", |a| a.context = Some(1)).await,
        "src/lib.rs-5- pub fn twice(x: i32) -> i32 {
src/lib.rs:6:     x * 2
src/lib.rs-7- }
--
src/lib.rs-9- pub fn thrice(x: i32) -> i32 {
src/lib.rs:10:     x * 3
src/lib.rs-11- }"
    );
}

#[tokio::test]
async fn grep_stops_at_100_matches_and_cuts_lines_at_500_characters() {
    let fx = Fixture::new().await;
    let wide = "hit ".repeat(200);
    fx.put("many.txt", &format!("{wide}\n").repeat(150));
    let found = fx.grep("hit", |_| {}).await;
    let lines: Vec<&str> = found.lines().collect();
    assert_eq!(lines.len(), 101);
    assert_eq!(lines[0], format!("many.txt:1: {}", &wide[..500]));
    assert_eq!(
        lines[100],
        "[showing 100 of 150 matches; narrow your search]"
    );
}

#[tokio::test]
async fn grep_says_when_nothing_matches_and_refuses_a_bad_pattern() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(fx.grep("nowhere at all", |_| {}).await, "No matches.");
    let bad = fx.grep("(", |_| {}).await;
    assert!(bad.starts_with("Err: bad pattern: "), "{bad}");
    let bad = fx.grep("fn", |a| a.glob = Some("**/{a".to_owned())).await;
    assert!(bad.starts_with("Err: bad glob: "), "{bad}");
}

// bash

#[tokio::test]
async fn bash_runs_in_the_work_dir_and_says_how_it_ended() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "");
    assert_eq!(fx.bash("ls; exit 3", None).await, "a.txt\n[exit code 3]");
    assert_eq!(fx.bash("true", None).await, "[exit code 0]");
}

#[tokio::test]
async fn bash_merges_stderr_into_stdout_in_order() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.bash("echo out; echo err >&2; echo again", None).await,
        "out\nerr\nagain\n[exit code 0]"
    );
}

#[tokio::test]
async fn bash_keeps_the_last_2000_lines_and_says_how_many_were_cut() {
    let fx = Fixture::new().await;
    let ran = fx.bash("seq 1 2500", None).await;
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(lines.len(), 2002);
    assert_eq!(lines[0], "[500 earlier lines cut]");
    assert_eq!(lines[1], "501");
    assert_eq!(lines[2000], "2500");
    assert_eq!(lines[2001], "[exit code 0]");
}

#[tokio::test]
async fn bash_past_its_timeout_is_stopped() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.bash("echo started; sleep 5", Some(1)).await,
        "started\n[timed out after 1s and was stopped]"
    );
    assert_eq!(
        fx.bash("sleep 1", Some(0)).await,
        "[timed out after 0s and was stopped]"
    );
}

#[test]
fn bash_names_a_shell_that_is_not_bash() {
    let bash = description(Some(&SandboxPath::new("/bin/bash").unwrap()));
    let sh = description(Some(&SandboxPath::new("/bin/sh").unwrap()));
    assert!(!bash.contains("not bash"), "{bash}");
    assert!(
        sh.contains("The shell is `/bin/sh`, not bash: write POSIX sh"),
        "{sh}"
    );
}
