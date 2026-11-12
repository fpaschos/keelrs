//! Built-in tools: they arrive with `use_sandbox`, first in the tool list,
//! and each instance's remember what they read.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use keelrs::sandbox::Sandbox;
use keelrs::testing::ScriptedModel;
use keelrs::{Agent, AgentInstance, InstanceId, Tool, ToolCall};
use parking_lot::Mutex;
use serde_json::json;
use tokio::time::timeout;

use crate::support::{
    TIMEOUT, TempDir, agent, clock, first_request, haiku, last_results, open, reason, runtime,
    sandboxed_agent, scripted, tool_names,
};

/// The built-in tools, in their order.
const BUILTINS: [&str; 8] = [
    "read", "write", "edit", "bash", "grep", "glob", "delete", "move",
];

#[tokio::test]
async fn use_sandbox_alone_declares_the_builtins_before_skill_tools_and_declared_tools() {
    let sb = open(&TempDir::new()).await;
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
    let sb = open(&TempDir::new()).await;
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
    let sb = open(&TempDir::new()).await;
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
    let sb = open(&TempDir::new()).await;
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
struct Switch {
    sb: Mutex<Sandbox>,
    broken: AtomicBool,
}

impl Switch {
    fn new(sb: &Sandbox) -> Arc<Self> {
        Arc::new(Self {
            sb: Mutex::new(sb.clone()),
            broken: AtomicBool::new(false),
        })
    }

    fn to(&self, sb: &Sandbox) {
        *self.sb.lock() = sb.clone();
    }
}

/// An agent with the built-ins, in the sandbox `switch` holds at each turn setup.
fn switching(switch: &Arc<Switch>) -> Agent {
    let switch = Arc::clone(switch);
    sandboxed_agent(move |cx| {
        let sb = switch.sb.lock().clone();
        let declared = cx.use_sandbox(&sb);
        if switch.broken.load(Ordering::Relaxed) {
            declared.only(["shell"]);
        }
    })
}

/// The model reads `path`, then is done.
fn read(model: ScriptedModel, path: &str) -> ScriptedModel {
    model
        .calls([("c1", "read", json!({"path": path}))])
        .reply("Done.")
}

/// The model edits `one` to `1` in `path`, then is done.
fn edit_one(model: ScriptedModel, path: &str) -> ScriptedModel {
    let edit = json!({"path": path, "edits": [{"old_text": "one", "new_text": "1"}]});
    model.calls([("c1", "edit", edit)]).reply("Done.")
}

/// Asks `instance` to go; what its last tool result was.
async fn go(instance: &AgentInstance, model: &ScriptedModel) -> String {
    let reply = timeout(TIMEOUT, instance.ask("Go")).await.unwrap();
    reply.unwrap();
    last_results(model).pop().unwrap()
}

#[tokio::test]
async fn each_instance_remembers_only_what_its_own_builtins_read() {
    let dir = TempDir::new();
    let switch = Switch::new(&open(&dir).await);
    let agent = switching(&switch);
    let model = edit_one(
        edit_one(read(ScriptedModel::new(), "a.txt"), "a.txt"),
        "a.txt",
    );
    let runtime = runtime(&agent, &model);
    let reader = runtime.instance(&agent, InstanceId::fresh()).unwrap();
    let other = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    dir.write("a.txt", "one\n");
    go(&reader, &model).await;
    dir.write("a.txt", "two\n");
    let theirs = go(&other, &model).await;
    assert!(!theirs.contains(STALE), "{theirs}");
    let mine = go(&reader, &model).await;
    assert!(mine.contains(STALE), "{mine}");
}

#[tokio::test]
async fn a_turn_in_another_sandbox_forgets_what_the_builtins_read() {
    let dir = TempDir::new();
    let (first, second) = (open(&dir).await, open(&dir).await);
    let switch = Switch::new(&first);
    let agent = switching(&switch);
    let model = edit_one(read(ScriptedModel::new(), "a.txt").reply("Done."), "a.txt");
    let runtime = runtime(&agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    dir.write("a.txt", "one\n");
    go(&instance, &model).await;
    dir.write("a.txt", "two\n");
    switch.to(&second);
    timeout(TIMEOUT, instance.ask("Go")).await.unwrap().unwrap();

    // Back in the first sandbox, the edit reads the file afresh, not the old read.
    switch.to(&first);
    let edited = go(&instance, &model).await;
    assert!(
        edited.starts_with("error:`old_text` was not found"),
        "{edited}"
    );
}

#[tokio::test]
async fn a_work_dir_of_the_same_sandbox_keeps_what_the_builtins_read() {
    let dir = TempDir::new();
    let root = open(&dir).await;
    let switch = Switch::new(&root);
    let agent = switching(&switch);
    let model = edit_one(read(ScriptedModel::new(), "sub/a.txt"), "a.txt");
    let runtime = runtime(&agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    dir.write("sub/a.txt", "one\n");
    go(&instance, &model).await;
    dir.write("sub/a.txt", "two\n");
    switch.to(&root.work_dir("sub"));
    let edited = go(&instance, &model).await;
    assert!(edited.contains(STALE), "{edited}");
}

#[tokio::test]
async fn a_failed_turn_setup_in_another_sandbox_keeps_what_the_builtins_read() {
    let dir = TempDir::new();
    let (first, second) = (open(&dir).await, open(&TempDir::new()).await);
    let switch = Switch::new(&first);
    let agent = switching(&switch);
    let model = edit_one(read(ScriptedModel::new(), "a.txt"), "a.txt");
    let runtime = runtime(&agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    dir.write("a.txt", "one\n");
    go(&instance, &model).await;
    dir.write("a.txt", "two\n");
    switch.to(&second);
    switch.broken.store(true, Ordering::Relaxed);
    let failed = timeout(TIMEOUT, instance.ask("Go")).await.unwrap();
    reason(failed);

    switch.to(&first);
    switch.broken.store(false, Ordering::Relaxed);
    let edited = go(&instance, &model).await;
    assert!(edited.contains(STALE), "{edited}");
}
