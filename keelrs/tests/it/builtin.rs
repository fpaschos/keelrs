//! Built-in tools: they arrive with `use_sandbox`, first in the tool list,
//! and each instance's remember what they read.

use std::convert::Infallible;
use std::sync::Arc;

use keelrs::sandbox::Sandbox;
use keelrs::testing::ScriptedModel;
use keelrs::{Agent, AgentInstance, InstanceId, Tool, ToolCall};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::support::{
    agent, ask, clock, edit, first_request, haiku, instance, last_results, open, reason,
    sandboxed_agent, scripted, temp_dir, tool_names, write,
};

/// The built-in tools, in their order.
const BUILTINS: [&str; 8] = [
    "read", "write", "edit", "bash", "grep", "glob", "delete", "move",
];

#[tokio::test]
async fn use_sandbox_alone_declares_the_builtins_before_skill_tools_and_declared_tools() {
    let sb = open(&temp_dir()).await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_tool(clock());
        cx.use_skill(haiku());
        cx.use_sandbox(&sb);
    });
    let expected: Vec<&str> = BUILTINS
        .into_iter()
        .chain(["activate_skill", "current_time"])
        .collect();
    assert_eq!(tool_names(&first_request(&agent).await), expected);
}

#[tokio::test]
async fn only_keeps_the_named_builtins_in_their_own_order() {
    let sb = open(&temp_dir()).await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only(["bash", "glob", "grep", "read"]);
    });
    assert_eq!(
        tool_names(&first_request(&agent).await),
        ["read", "bash", "grep", "glob"]
    );
}

#[tokio::test]
async fn an_unknown_name_in_only_fails_the_run() {
    let sb = open(&temp_dir()).await;
    let agent = sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only(["read", "shell"]);
    });
    let failed = scripted(&agent, &ScriptedModel::new(), "Go").await;
    assert_eq!(
        reason(failed),
        "`only` names `shell`, which is not a built-in tool; they are \
         read, write, edit, bash, grep, glob, delete, move"
    );
}

#[tokio::test]
async fn a_builtin_name_is_reserved_with_or_without_only() {
    let sb = open(&temp_dir()).await;
    for name in BUILTINS {
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
        let left_out = agent(move |cx| cx.use_tool(mine.clone())).await;
        for agent in [all, left_out] {
            let failed = scripted(&agent, &ScriptedModel::new(), "Go").await;
            assert_eq!(
                reason(failed),
                format!("`{name}` is reserved for keelrs's built-in tool")
            );
        }
    }
}

const STALE: &str = "changed since it was last read";

/// The sandbox an agent's next turn setup declares, and whether it also
/// declares an unknown built-in, so its turn setup fails.
type Switch = Arc<Mutex<(Sandbox, bool)>>;

/// An agent with the built-ins, in the sandbox `switch` holds at each turn setup.
fn switching(sb: &Sandbox) -> (Agent, Switch) {
    let switch: Switch = Arc::new(Mutex::new((sb.clone(), false)));
    let agent = sandboxed_agent({
        let switch = Arc::clone(&switch);
        move |cx| {
            let (sb, broken) = switch.lock().clone();
            let declared = cx.use_sandbox(&sb);
            if broken {
                declared.only(["shell"]);
            }
        }
    });
    (agent, switch)
}

/// A call that reads `path`.
fn read(path: &str) -> (&'static str, &'static str, Value) {
    ("c1", "read", json!({"path": path}))
}

/// A call that edits `one` to `1` in `path`.
fn edit_one(path: &str) -> (&'static str, &'static str, Value) {
    ("c1", "edit", edit(path, "one", "1"))
}

/// Asks `instance` to go; what its last tool result was.
async fn go(instance: &AgentInstance, model: &ScriptedModel) -> String {
    ask(instance, "Go").await.unwrap();
    last_results(model).pop().unwrap()
}

#[tokio::test]
async fn each_instance_remembers_only_what_its_own_builtins_read() {
    let dir = temp_dir();
    let (agent, _) = switching(&open(&dir).await);
    let model = ScriptedModel::new()
        .calls([read("a.txt")])
        .reply("Done.")
        .calls([edit_one("a.txt")])
        .reply("Done.")
        .calls([edit_one("a.txt")])
        .reply("Done.");
    let (runtime, reader) = instance(&agent, &model);
    let other = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    write(&dir, "a.txt", "one\n");
    go(&reader, &model).await;
    write(&dir, "a.txt", "two\n");
    let theirs = go(&other, &model).await;
    assert!(!theirs.contains(STALE), "{theirs}");
    let mine = go(&reader, &model).await;
    assert!(mine.contains(STALE), "{mine}");
}

#[tokio::test]
async fn a_turn_in_another_sandbox_forgets_what_the_builtins_read() {
    let dir = temp_dir();
    let (first, second) = (open(&dir).await, open(&dir).await);
    let (agent, switch) = switching(&first);
    let model = ScriptedModel::new()
        .calls([read("a.txt")])
        .reply("Done.")
        .reply("Done.")
        .calls([edit_one("a.txt")])
        .reply("Done.");
    let (_runtime, instance) = instance(&agent, &model);

    write(&dir, "a.txt", "one\n");
    go(&instance, &model).await;
    write(&dir, "a.txt", "two\n");
    *switch.lock() = (second, false);
    ask(&instance, "Go").await.unwrap();

    // Back in the first sandbox, the edit reads the file afresh, not the old read.
    *switch.lock() = (first, false);
    let edited = go(&instance, &model).await;
    assert!(
        edited.starts_with("error:`old_text` was not found"),
        "{edited}"
    );
}

#[tokio::test]
async fn a_work_dir_of_the_same_sandbox_keeps_what_the_builtins_read() {
    let dir = temp_dir();
    let root = open(&dir).await;
    let (agent, switch) = switching(&root);
    let model = ScriptedModel::new()
        .calls([read("sub/a.txt")])
        .reply("Done.")
        .calls([edit_one("a.txt")])
        .reply("Done.");
    let (_runtime, instance) = instance(&agent, &model);

    write(&dir, "sub/a.txt", "one\n");
    go(&instance, &model).await;
    write(&dir, "sub/a.txt", "two\n");
    *switch.lock() = (root.work_dir("sub"), false);
    let edited = go(&instance, &model).await;
    assert!(edited.contains(STALE), "{edited}");
}

#[tokio::test]
async fn a_failed_turn_setup_in_another_sandbox_keeps_what_the_builtins_read() {
    let dir = temp_dir();
    let (first, second) = (open(&dir).await, open(&temp_dir()).await);
    let (agent, switch) = switching(&first);
    let model = ScriptedModel::new()
        .calls([read("a.txt")])
        .reply("Done.")
        .calls([edit_one("a.txt")])
        .reply("Done.");
    let (_runtime, instance) = instance(&agent, &model);

    write(&dir, "a.txt", "one\n");
    go(&instance, &model).await;
    write(&dir, "a.txt", "two\n");
    *switch.lock() = (second, true);
    reason(ask(&instance, "Go").await);

    *switch.lock() = (first, false);
    let edited = go(&instance, &model).await;
    assert!(edited.contains(STALE), "{edited}");
}
