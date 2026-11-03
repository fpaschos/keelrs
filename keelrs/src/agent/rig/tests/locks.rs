//! One reply's calls on one file run one after the other: the built-in
//! file tools and a user's tool hold the file's `Sandbox::lock`.

use super::*;

/// Runs `turns` on `agent`, failing if it takes over five seconds.
async fn in_time(agent: &Agent, turns: impl IntoIterator<Item = MockTurn>) -> Settled {
    tokio::time::timeout(Duration::from_secs(5), scripted(agent, turns))
        .await
        .expect("the reply's calls deadlocked")
        .unwrap()
}

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
            Ok::<_, crate::Error>("appended")
        },
    )
}

/// An agent on the mock model with the built-ins and `append_slowly(path)`, through `sb`.
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

#[tokio::test]
async fn a_users_locked_tool_and_an_edit_of_one_file_both_land() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\n");
    let settled = in_time(
        &appending_in(&sb, "f"),
        [
            reply([
                ("c1", "append", json!({})),
                ("c2", "edit", edit("f", "one", "1")),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    assert_eq!(folder.read("f").unwrap(), "1\ntwo\n");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn a_users_locked_tool_and_a_write_of_one_file_run_one_after_the_other() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\n");
    let write = json!({"path": "f", "content": "new\n"});
    let settled = in_time(
        &appending_in(&sb, "f"),
        [
            reply([("c1", "append", json!({})), ("c2", "write", write)]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    // Append first, then the write replaces the file whole; or the write
    // first, then the append. Never the append over the text it read before.
    let text = folder.read("f").unwrap();
    assert!(text == "new\n" || text == "new\ntwo\n", "{text:?}");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn two_edits_of_one_file_from_the_same_read_both_land() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\ntwo\n");
    let settled = in_time(
        &agent_in(&sb),
        [
            MockTurn::tool_call("c0", "read", json!({"path": "f"})),
            reply([
                ("c1", "edit", edit("f", "one", "1")),
                ("c2", "edit", edit("f", "two", "2")),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    assert_eq!(folder.read("f").unwrap(), "1\n2\n");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn a_failed_edit_releases_its_file() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\n");
    let settled = in_time(
        &agent_in(&sb),
        [
            reply([
                ("c1", "edit", edit("f", "absent", "1")),
                ("c2", "edit", edit("f", "one", "1")),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results[0].1 && !results[1].1, "{results:?}");
    assert_eq!(folder.read("f").unwrap(), "1\n");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn an_edit_beside_a_move_of_its_file_lands_before_it_or_finds_it_gone() {
    let (folder, sb) = sandboxed().await;
    folder.write("a", "one\n");
    let settled = in_time(
        &agent_in(&sb),
        [
            reply([
                ("c1", "move", json!({"from": "a", "to": "c"})),
                ("c2", "edit", edit("a", "one", "1")),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(!results[0].1, "{results:?}");
    assert_eq!(folder.read("a"), None, "the edit recreated a");
    match folder.read("c").unwrap().as_str() {
        "1\n" => assert!(!results[1].1, "{results:?}"),
        "one\n" => assert!(results[1].1, "{results:?}"),
        other => panic!("{other:?}: {results:?}"),
    }
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn a_work_dir_and_its_sandbox_lock_the_same_file() {
    let (folder, sb) = sandboxed().await;
    folder.write("sub/f", "one\n");
    let (root, sub) = (appending_in(&sb, "sub/f"), agent_in(&sb.work_dir("sub")));
    let appending = in_time(
        &root,
        [
            reply([("c1", "append", json!({}))]),
            MockTurn::text("Done."),
        ],
    );
    let editing = in_time(
        &sub,
        [
            reply([("c1", "edit", edit("f", "one", "1"))]),
            MockTurn::text("Done."),
        ],
    );
    let (appended, edited) = tokio::join!(appending, editing);
    let results = [results(&appended), results(&edited)].concat();
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    assert_eq!(folder.read("sub/f").unwrap(), "1\ntwo\n");
    assert_eq!(sb.locked(), 0);
}
