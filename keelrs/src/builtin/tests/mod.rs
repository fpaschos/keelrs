//! The built-in tools against `sandbox::local()` on a temp folder, each call
//! run as a turn runs it (`agent::rig::call`): each tool's text and refusals.
//! How they are declared for a turn is tested with the tool loop.
//! The fixture is here, the tests one file per tool in the child modules.

#![allow(clippy::disallowed_methods)] // the fixture files are set up on the host

use std::fs;

use rig_tools::Seen;
use serde_json::{Value, json};

use crate::agent::rig::call;
use crate::sandbox::{self, Sandbox, SandboxPath};
use crate::tool::{Called, FailKind};
use assert_fs::TempDir;
use assert_fs::prelude::*;

mod bash;
mod edit;
mod files;
mod glob;
mod grep;
mod read;
mod write;

/// A temp folder with a local sandbox rooted in it, removed on drop.
struct Fixture {
    dir: TempDir,
    sb: Sandbox,
    seen: Seen,
}

impl Fixture {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let sb = sandbox::local().root(dir.path()).open().await.unwrap();
        Self {
            dir,
            sb,
            seen: Seen::default(),
        }
    }

    /// Writes `contents` at `relative` on the host, creating its folders.
    fn put(&self, relative: &str, contents: &str) -> &Self {
        self.dir.child(relative).write_str(contents).unwrap();
        self
    }

    /// What the host holds at `relative`.
    fn get(&self, relative: &str) -> String {
        fs::read_to_string(self.dir.join(relative)).unwrap()
    }

    /// One call of the built-in `name`, run as a turn runs it.
    async fn call(&self, name: &str, arguments: Value) -> Called {
        let tool = super::tools(&self.sb, &self.seen, &[name]).remove(0);
        call(&tool, arguments, "c1".to_owned(), &self.sb).await
    }

    async fn read(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> String {
        let args = json!({"path": path, "offset": offset, "limit": limit});
        shown(self.call("read", args).await)
    }

    async fn write(&self, path: &str, content: &str) -> String {
        shown(
            self.call("write", json!({"path": path, "content": content}))
                .await,
        )
    }

    /// One `edit` call of `path`, each edit `(old_text, new_text, replace_all)`.
    async fn edit(&self, path: &str, edits: &[(&str, &str, bool)]) -> String {
        let edits: Vec<Value> = edits
            .iter()
            .map(|&(old, new, all)| json!({"old_text": old, "new_text": new, "replace_all": all}))
            .collect();
        shown(
            self.call("edit", json!({"path": path, "edits": edits}))
                .await,
        )
    }

    async fn delete(&self, path: &str) -> String {
        shown(self.call("delete", json!({"path": path})).await)
    }

    async fn move_file(&self, from: &str, to: &str, overwrite: bool) -> String {
        let args = json!({"from": from, "to": to, "overwrite": overwrite});
        shown(self.call("move", args).await)
    }

    fn exists(&self, relative: &str) -> bool {
        self.dir.join(relative).exists()
    }

    async fn glob(&self, pattern: &str, path: Option<&str>, hidden: bool) -> String {
        let args = json!({"pattern": pattern, "path": path, "hidden": hidden});
        shown(self.call("glob", args).await)
    }

    /// One `grep` call of `pattern`, with the arguments in `more` besides it.
    async fn grep(&self, pattern: &str, mut more: Value) -> String {
        more["pattern"] = json!(pattern);
        shown(self.call("grep", more).await)
    }

    async fn bash(&self, command: &str, timeout: Option<u64>) -> String {
        shown(
            self.call("bash", json!({"command": command, "timeout": timeout}))
                .await,
        )
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
fn shown(called: Called) -> String {
    match called {
        Ok(Value::String(text)) => text,
        Ok(other) => panic!("a built-in returns text, not {other}"),
        Err(failed) => format!("Err: {failed}"),
    }
}
