//! Built-in tools: they arrive with `use_sandbox`, first in the tool list.

use super::*;

/// A local sandbox on a fresh temp folder, which is removed on drop.
async fn sandboxed() -> (Folder, Sandbox) {
    let folder = Folder::new();
    let sb = crate::sandbox::local()
        .root(&folder.0)
        .open()
        .await
        .unwrap();
    (folder, sb)
}

#[tokio::test]
async fn use_sandbox_alone_declares_the_builtins_before_skill_tools_and_declared_tools() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_tool(clock());
        cx.use_skill(haiku());
        cx.use_sandbox(&sb);
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    let expected: Vec<&str> = crate::builtin::NAMES
        .into_iter()
        .chain(["activate_skill", "current_time"])
        .collect();
    assert_eq!(tool_names(&requests[0]), expected);
}

#[tokio::test]
async fn only_keeps_the_named_builtins_in_their_own_order() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only(["bash", "glob", "grep", "read"]);
    });
    let (_, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(tool_names(&requests[0]), ["read", "bash", "grep", "glob"]);
}

#[tokio::test]
async fn an_unknown_name_in_only_fails_the_run() {
    let (_folder, sb) = sandboxed().await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only(["read", "shell"]);
    });
    let failed = scripted(&agent, [MockTurn::text("Done.")])
        .await
        .unwrap_err();
    assert_eq!(
        failed,
        "`only` names `shell`, which is not a built-in tool; they are \
         read, write, edit, bash, grep, glob, delete, move"
    );
}

#[tokio::test]
async fn a_builtin_name_is_reserved_with_or_without_only() {
    let (_folder, sb) = sandboxed().await;
    for name in crate::builtin::NAMES {
        let mine = Tool::host(name, "Mine.", async |_: ToolCall<()>| {
            Ok::<_, Infallible>("ran")
        });
        let all = sandboxed_agent({
            let (sb, mine) = (sb.clone(), mine.clone());
            move |cx| {
                cx.use_sandbox(&sb);
                cx.use_tool(mine.clone());
            }
        });
        let left_out = agent(move |cx| cx.use_tool(mine.clone()));
        for agent in [all, left_out] {
            let failed = scripted(&agent, [MockTurn::text("Done.")])
                .await
                .unwrap_err();
            assert_eq!(
                failed,
                format!("`{name}` is reserved for keelrs's built-in tool")
            );
        }
    }
}

/// An agent on the mock model whose built-ins act through `sb`.
fn agent_in(sb: &Sandbox) -> Agent {
    let sb = sb.clone();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb);
    })
}

/// The model reads `path`, then is done.
fn read(path: &str) -> [MockTurn; 2] {
    [
        MockTurn::tool_call("c1", "read", json!({"path": path})),
        MockTurn::text("Done."),
    ]
}

/// The model edits `one` to `1` in `path`, then is done.
fn edit_one(path: &str) -> [MockTurn; 2] {
    [
        MockTurn::tool_call(
            "c1",
            "edit",
            json!({"path": path, "edits": [{"old_text": "one", "new_text": "1"}]}),
        ),
        MockTurn::text("Done."),
    ]
}

const STALE: &str = "changed since it was last read";

#[tokio::test]
async fn each_instance_remembers_only_what_its_own_builtins_read() {
    let (folder, sb) = sandboxed().await;
    folder.write("a.txt", "one\n");
    let (mut reader, mut other) = (Caches::default(), Caches::default());
    scripted_in(&agent_in(&sb), read("a.txt"), &mut reader)
        .await
        .unwrap();
    folder.write("a.txt", "two\n");

    let theirs = results(
        &scripted_in(&agent_in(&sb), edit_one("a.txt"), &mut other)
            .await
            .unwrap(),
    );
    assert!(!theirs[0].0.contains(STALE), "{theirs:?}");
    let mine = results(
        &scripted_in(&agent_in(&sb), edit_one("a.txt"), &mut reader)
            .await
            .unwrap(),
    );
    assert!(mine[0].0.contains(STALE), "{mine:?}");
}

#[tokio::test]
async fn a_render_in_another_sandbox_forgets_what_the_builtins_read() {
    let (folder, first) = sandboxed().await;
    let second = crate::sandbox::local()
        .root(&folder.0)
        .open()
        .await
        .unwrap();
    folder.write("a.txt", "one\n");
    let mut caches = Caches::default();
    scripted_in(&agent_in(&first), read("a.txt"), &mut caches)
        .await
        .unwrap();
    folder.write("a.txt", "two\n");
    scripted_in(&agent_in(&second), [MockTurn::text("Done.")], &mut caches)
        .await
        .unwrap();

    // Back in the first sandbox, the edit reads the file afresh, not the old read.
    let edited = scripted_in(&agent_in(&first), edit_one("a.txt"), &mut caches)
        .await
        .unwrap();
    let edited = results(&edited);
    assert!(
        edited[0].0.starts_with("`old_text` was not found"),
        "{edited:?}"
    );
}

#[tokio::test]
async fn a_work_dir_of_the_same_sandbox_keeps_what_the_builtins_read() {
    let (folder, root) = sandboxed().await;
    folder.write("sub/a.txt", "one\n");
    let mut caches = Caches::default();
    scripted_in(&agent_in(&root), read("sub/a.txt"), &mut caches)
        .await
        .unwrap();
    folder.write("sub/a.txt", "two\n");

    let sub = root.work_dir("sub");
    let edited = scripted_in(&agent_in(&sub), edit_one("a.txt"), &mut caches)
        .await
        .unwrap();
    let edited = results(&edited);
    assert!(edited[0].0.contains(STALE), "{edited:?}");
}

#[tokio::test]
async fn a_failed_render_in_another_sandbox_keeps_what_the_builtins_read() {
    let (folder, first) = sandboxed().await;
    folder.write("a.txt", "one\n");
    let mut caches = Caches::default();
    scripted_in(&agent_in(&first), read("a.txt"), &mut caches)
        .await
        .unwrap();
    folder.write("a.txt", "two\n");
    let unknown = sandboxed_agent(|cx| {
        cx.use_sandbox(&unused()).only(["shell"]);
    });
    scripted_in(&unknown, [MockTurn::text("Done.")], &mut caches)
        .await
        .unwrap_err();

    let edited = scripted_in(&agent_in(&first), edit_one("a.txt"), &mut caches)
        .await
        .unwrap();
    let edited = results(&edited);
    assert!(edited[0].0.contains(STALE), "{edited:?}");
}

// One reply's calls on one file run one after the other.

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
