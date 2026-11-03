//! One reply's calls on one file run one after the other: the built-in
//! file tools and a user's tool hold the file's `Sandbox::lock`.

use super::*;

/// What `folder` holds at `relative`.
#[allow(clippy::disallowed_methods)] // the fixture is read on the host
fn held(folder: &Folder, relative: &str) -> String {
    std::fs::read_to_string(folder.0.join(relative)).unwrap()
}

/// Runs `turns` on `agent`, failing if it takes over five seconds.
async fn in_time(agent: &Agent, turns: impl IntoIterator<Item = MockTurn>) -> Settled {
    tokio::time::timeout(Duration::from_secs(5), scripted(agent, turns))
        .await
        .expect("the reply's calls deadlocked")
        .unwrap()
}

#[derive(Deserialize, JsonSchema)]
struct Line {
    line: String,
}

/// A tool of the user's that appends a line to `f` under `sb.lock`, slowly.
fn append_slowly() -> Tool {
    Tool::sandbox(
        "append",
        "Append a line to f.",
        async |call: ToolCall<Line>, sb: Sandbox| {
            let _held = sb.lock("f").await?;
            let text = sb.read("f", u64::MAX).await?.bytes;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut text = String::from_utf8(text).unwrap();
            text.push_str(&call.data.line);
            sb.write("f", text).await?;
            Ok::<_, crate::Error>("appended")
        },
    )
}

#[tokio::test]
async fn a_users_locked_tool_and_an_edit_of_one_file_both_land() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\n");
    let agent = sandboxed_agent({
        let sb = sb.clone();
        move |cx| {
            cx.use_sandbox(&sb);
            cx.use_tool(append_slowly());
        }
    });
    let edit = json!({"path": "f", "edits": [{"old_text": "one", "new_text": "1"}]});
    let settled = in_time(
        &agent,
        [
            reply([
                ("c1", "append", json!({"line": "two\n"})),
                ("c2", "edit", edit),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    assert_eq!(held(&folder, "f"), "1\ntwo\n");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn two_edits_of_one_file_from_the_same_read_both_land() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\ntwo\n");
    let edit =
        |old: &str, new: &str| json!({"path": "f", "edits": [{"old_text": old, "new_text": new}]});
    let settled = in_time(
        &agent_in(&sb),
        [
            MockTurn::tool_call("c0", "read", json!({"path": "f"})),
            reply([
                ("c1", "edit", edit("one", "1")),
                ("c2", "edit", edit("two", "2")),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    assert_eq!(held(&folder, "f"), "1\n2\n");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn a_write_and_an_edit_of_one_file_run_one_after_the_other() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\n");
    let settled = in_time(
        &agent_in(&sb),
        [
            reply([
                ("c1", "write", json!({"path": "f", "content": "one two\n"})),
                (
                    "c2",
                    "edit",
                    json!({"path": "f", "edits": [{"old_text": "two", "new_text": "2"}]}),
                ),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    // Edit second: on the written text. Edit first: "two" is not found, and
    // the write replaces the file whole.
    match held(&folder, "f").as_str() {
        "one 2\n" => assert!(!results[1].1, "{results:?}"),
        "one two\n" => assert!(results[1].1, "{results:?}"),
        other => panic!("{other:?}: {results:?}"),
    }
    assert!(!results[0].1, "{results:?}");
}

#[tokio::test]
async fn a_failed_edit_releases_its_file() {
    let (folder, sb) = sandboxed().await;
    folder.write("f", "one\n");
    let edit = |old: &str| json!({"path": "f", "edits": [{"old_text": old, "new_text": "1"}]});
    let settled = in_time(
        &agent_in(&sb),
        [
            reply([("c1", "edit", edit("absent")), ("c2", "edit", edit("one"))]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    let results = results(&settled);
    assert!(results[0].1 && !results[1].1, "{results:?}");
    assert_eq!(held(&folder, "f"), "1\n");
    assert_eq!(sb.locked(), 0);
}

#[tokio::test]
async fn a_move_beside_an_edit_or_a_move_back_does_not_deadlock() {
    let (folder, sb) = sandboxed().await;
    folder.write("a", "one\n").write("b", "two\n");
    let mv = |from: &str, to: &str| json!({"from": from, "to": to, "overwrite": true});
    in_time(
        &agent_in(&sb),
        [
            reply([
                ("c1", "move", mv("a", "c")),
                (
                    "c2",
                    "edit",
                    json!({"path": "a", "edits": [{"old_text": "one", "new_text": "1"}]}),
                ),
                ("c3", "move", mv("b", "d")),
                ("c4", "move", mv("d", "b")),
            ]),
            MockTurn::text("Done."),
        ],
    )
    .await;
    assert_eq!(sb.locked(), 0);
}
