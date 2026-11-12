//! One reply's calls on one file run one after the other: the built-in
//! file tools and a user's tool hold the file's `Sandbox::lock`.

use std::time::Duration;

use keelrs::sandbox::Sandbox;
use keelrs::testing::ScriptedModel;
use keelrs::{Agent, Tool, ToolCall};
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::support::{TempDir, last_results, open, sandboxed_agent, scripted};

/// A tool of the user's that appends `two` to `path` under `sb.lock`, slowly:
/// a change made between its read and its write would be lost.
fn append_slowly(path: &'static str) -> Tool {
    Tool::sandbox(
        "append",
        "Append `two` to a file.",
        async move |_: ToolCall<()>, sb: Sandbox| {
            let _held = sb.lock(path).await?;
            let text = sb.read(path, u64::MAX).await?.bytes;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut text = String::from_utf8(text).unwrap();
            text.push_str("two\n");
            sb.write(path, text).await?;
            Ok::<_, keelrs::Error>("appended")
        },
    )
}

/// An agent with the built-ins, acting through `sb`.
fn agent_in(sb: &Sandbox) -> Agent {
    let sb = sb.clone();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb);
    })
}

/// An agent with the built-ins and `append_slowly(path)`, through `sb`.
fn appending_in(sb: &Sandbox, path: &'static str) -> Agent {
    let sb = sb.clone();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb);
        cx.use_tool(append_slowly(path));
    })
}

/// One `edit` of `path`, replacing `old` with `new`.
fn edit(path: &str, old: &str, new: &str) -> Value {
    json!({"path": path, "edits": [{"old_text": old, "new_text": new}]})
}

/// Runs `agent` on `model`, which ends with "Done."; the tool results the
/// model got last. `scripted` times out if the calls deadlock.
async fn results(agent: &Agent, model: ScriptedModel) -> Vec<String> {
    let model = model.reply("Done.");
    scripted(agent, &model, "Go").await.unwrap();
    last_results(&model)
}

fn all_ok(results: &[String]) -> bool {
    results.iter().all(|r| r.starts_with("result:"))
}

/// Whether `path` in `sb` is free: every call released it.
async fn free(sb: &Sandbox, path: &str) -> bool {
    timeout(Duration::from_secs(1), sb.lock(path)).await.is_ok()
}

#[tokio::test]
async fn a_users_locked_tool_and_an_edit_of_one_file_both_land() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    dir.write("f", "one\n");
    let model = ScriptedModel::new().calls([
        ("c1", "append", json!({})),
        ("c2", "edit", edit("f", "one", "1")),
    ]);
    let results = results(&appending_in(&sb, "f"), model).await;
    assert!(all_ok(&results), "{results:?}");
    assert_eq!(dir.read("f").unwrap(), "1\ntwo\n");
    assert!(free(&sb, "f").await);
}

#[tokio::test]
async fn a_users_locked_tool_and_a_write_of_one_file_run_one_after_the_other() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    dir.write("f", "one\n");
    let write = json!({"path": "f", "content": "new\n"});
    let model = ScriptedModel::new().calls([("c1", "append", json!({})), ("c2", "write", write)]);
    let results = results(&appending_in(&sb, "f"), model).await;
    assert!(all_ok(&results), "{results:?}");
    // Append first, then the write replaces the file whole; or the write
    // first, then the append. Never the append over the text it read before.
    let text = dir.read("f").unwrap();
    assert!(text == "new\n" || text == "new\ntwo\n", "{text:?}");
    assert!(free(&sb, "f").await);
}

#[tokio::test]
async fn two_edits_of_one_file_from_the_same_read_both_land() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    dir.write("f", "one\ntwo\n");
    let model = ScriptedModel::new()
        .calls([("c0", "read", json!({"path": "f"}))])
        .calls([
            ("c1", "edit", edit("f", "one", "1")),
            ("c2", "edit", edit("f", "two", "2")),
        ]);
    let results = results(&agent_in(&sb), model).await;
    assert!(all_ok(&results), "{results:?}");
    assert_eq!(dir.read("f").unwrap(), "1\n2\n");
    assert!(free(&sb, "f").await);
}

#[tokio::test]
async fn a_failed_edit_releases_its_file() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    dir.write("f", "one\n");
    let model = ScriptedModel::new().calls([
        ("c1", "edit", edit("f", "absent", "1")),
        ("c2", "edit", edit("f", "one", "1")),
    ]);
    let results = results(&agent_in(&sb), model).await;
    assert!(
        results[0].starts_with("error:") && results[1].starts_with("result:"),
        "{results:?}"
    );
    assert_eq!(dir.read("f").unwrap(), "1\n");
    assert!(free(&sb, "f").await);
}

#[tokio::test]
async fn an_edit_beside_a_move_of_its_file_lands_before_it_or_finds_it_gone() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    dir.write("a", "one\n");
    let model = ScriptedModel::new().calls([
        ("c1", "move", json!({"from": "a", "to": "c"})),
        ("c2", "edit", edit("a", "one", "1")),
    ]);
    let results = results(&agent_in(&sb), model).await;
    assert!(results[0].starts_with("result:"), "{results:?}");
    assert_eq!(dir.read("a"), None, "the edit recreated a");
    match dir.read("c").unwrap().as_str() {
        "1\n" => assert!(results[1].starts_with("result:"), "{results:?}"),
        "one\n" => assert!(results[1].starts_with("error:"), "{results:?}"),
        other => panic!("{other:?}: {results:?}"),
    }
    assert!(free(&sb, "a").await && free(&sb, "c").await);
}

#[tokio::test]
async fn a_work_dir_and_its_sandbox_lock_the_same_file() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    dir.write("sub/f", "one\n");
    let (root, sub) = (appending_in(&sb, "sub/f"), agent_in(&sb.work_dir("sub")));
    let appending = results(
        &root,
        ScriptedModel::new().calls([("c1", "append", json!({}))]),
    );
    let editing = results(
        &sub,
        ScriptedModel::new().calls([("c1", "edit", edit("f", "one", "1"))]),
    );
    let (appended, edited) = tokio::join!(appending, editing);
    let results = [appended, edited].concat();
    assert!(all_ok(&results), "{results:?}");
    assert_eq!(dir.read("sub/f").unwrap(), "1\ntwo\n");
    assert!(free(&sb, "sub/f").await);
}
