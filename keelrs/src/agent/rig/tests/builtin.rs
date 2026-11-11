//! Built-in tools: they arrive with `use_sandbox`, first in the tool list.

use super::*;

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
async fn a_turn_in_another_sandbox_forgets_what_the_builtins_read() {
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
async fn a_failed_turn_setup_in_another_sandbox_keeps_what_the_builtins_read() {
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
