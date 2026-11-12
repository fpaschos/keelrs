//! What the tests share: temp dirs, sandboxes on them, and agents on a scripted model.

use std::convert::Infallible;
use std::time::Duration;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use keelrs::sandbox::{self, Sandbox};
use keelrs::store::{Store, StoreError};
use keelrs::testing::{Request, ScriptedModel};
use keelrs::{
    Agent, AgentContext, AgentInstance, Error, InstanceId, Outcome, Reply, Runtime, Skill, Tool,
    ToolCall,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::timeout;

/// A fresh dir under the system temp dir, removed on drop. Its path is
/// canonical (on macOS the temp dir is behind a symlink), as a sandbox sees it.
pub(crate) fn temp_dir() -> TempDir {
    TempDir::new_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

/// Writes `contents` at `relative` in `dir`, creating its folders.
pub(crate) fn write(dir: &TempDir, relative: &str, contents: &str) {
    dir.child(relative).write_str(contents).unwrap();
}

/// Writes `<root>/<folder>/SKILL.md` in `dir`, naming the skill `name`.
pub(crate) fn skill(dir: &TempDir, root: &str, folder: &str, name: &str, description: &str) {
    let text = format!("---\nname: {name}\ndescription: {description}\n---\nBody of {name}.\n");
    write(dir, &format!("{root}/{folder}/SKILL.md"), &text);
}

/// A local sandbox rooted at `dir`.
pub(crate) async fn open(dir: &TempDir) -> Sandbox {
    sandbox::local().root(dir.path()).open().await.unwrap()
}

/// How long a test waits for a dispatch that should settle.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn is_aborted<T>(result: &keelrs::Result<T>) -> bool {
    matches!(
        result,
        Err(Error::Run {
            outcome: Outcome::Aborted,
            ..
        })
    )
}

pub(crate) fn is_failed<T>(result: &keelrs::Result<T>) -> bool {
    matches!(
        result,
        Err(Error::Run {
            outcome: Outcome::Failed { .. },
            ..
        })
    )
}

/// Waits until `model` got `n` requests.
pub(crate) async fn until_requests(model: &ScriptedModel, n: usize) {
    timeout(TIMEOUT, async {
        while model.requests().len() < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the model is called");
}

/// Runs `call` until a dropped runtime no longer holds the instance's lease.
pub(crate) async fn once_free<T, F>(mut call: impl FnMut() -> F) -> keelrs::Result<T>
where
    F: Future<Output = keelrs::Result<T>>,
{
    for _ in 0..100 {
        match call().await {
            Err(Error::Store(StoreError::Held { .. })) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            other => return other,
        }
    }
    panic!("the lease was never released");
}

/// An agent on the scripted model whose `declare` calls `use_sandbox` itself.
pub(crate) fn sandboxed_agent(
    declare: impl Fn(&mut AgentContext) + Send + Sync + 'static,
) -> Agent {
    Agent::new("scripted", move |cx: &mut AgentContext| {
        // The runtime's scripted model answers in its place.
        cx.use_model("scripted/model");
        declare(cx);
        String::new()
    })
}

/// An agent on the scripted model with no built-ins, in a sandbox (the
/// system temp dir) its tools never touch.
pub(crate) async fn agent(declare: impl Fn(&mut AgentContext) + Send + Sync + 'static) -> Agent {
    let sb = sandbox::local()
        .root(std::env::temp_dir())
        .open()
        .await
        .unwrap();
    sandboxed_agent(move |cx| {
        cx.use_sandbox(&sb).only([]);
        declare(cx);
    })
}

/// A runtime on `store` hosting `agent`, its turns answered by `model`.
pub(crate) fn runtime_on(store: &Store, agent: &Agent, model: &ScriptedModel) -> Runtime {
    Runtime::builder()
        .store(store.clone())
        .model(model.clone())
        .start([agent])
        .unwrap()
}

/// A fresh instance of `agent` on a runtime of its own (dropped with it),
/// its turns answered by `model`.
pub(crate) fn instance(agent: &Agent, model: &ScriptedModel) -> (Runtime, AgentInstance) {
    let runtime = runtime_on(&Store::in_memory(), agent, model);
    let instance = runtime.instance(agent, InstanceId::fresh()).unwrap();
    (runtime, instance)
}

/// Asks `instance` `message`, failing the test if it does not settle in time.
pub(crate) async fn ask(instance: &AgentInstance, message: &str) -> keelrs::Result<Reply> {
    timeout(TIMEOUT, instance.ask(message))
        .await
        .expect("the dispatch settles")
}

/// Asks a fresh instance of `agent` `message`, answered by `model`; the reply's text.
pub(crate) async fn scripted(
    agent: &Agent,
    model: &ScriptedModel,
    message: &str,
) -> keelrs::Result<String> {
    let (_runtime, instance) = instance(agent, model);
    let reply = ask(&instance, message).await?;
    Ok(reply.text().to_owned())
}

pub(crate) fn clock() -> Tool {
    Tool::host(
        "current_time",
        "The current time.",
        async |_: ToolCall<()>| Ok::<_, Infallible>("noon"),
    )
}

#[derive(Deserialize, JsonSchema)]
struct Order {
    order_id: String,
}

pub(crate) fn lookup_order() -> Tool {
    Tool::host(
        "lookup_order",
        "Look up an order.",
        async |call: ToolCall<Order>| {
            Ok::<_, Infallible>(json!({"order": call.data.order_id, "call": call.id}))
        },
    )
}

/// The reason a dispatch failed with.
pub(crate) fn reason<T: std::fmt::Debug>(result: keelrs::Result<T>) -> String {
    match result {
        Err(Error::Run {
            outcome: Outcome::Failed { reason },
            ..
        }) => reason,
        other => panic!("expected a failed run, got {other:?}"),
    }
}

/// The tool results among `messages`, in order: `result:<text>` or `error:<text>`.
pub(crate) fn tool_results(messages: &[String]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.starts_with("result:") || m.starts_with("error:"))
        .cloned()
        .collect()
}

/// Every tool result the model's last request carried.
pub(crate) fn last_results(model: &ScriptedModel) -> Vec<String> {
    let requests = model.requests();
    tool_results(&requests.last().expect("the model was called").messages)
}

/// The arguments of one `edit` of `path`, replacing `old` with `new`.
pub(crate) fn edit(path: &str, old: &str, new: &str) -> Value {
    json!({"path": path, "edits": [{"old_text": old, "new_text": new}]})
}

/// What `agent`'s first turn sent a model that replies "Done.".
pub(crate) async fn first_request(agent: &Agent) -> Request {
    let model = ScriptedModel::new().reply("Done.");
    scripted(agent, &model, "Go").await.unwrap();
    model.requests().remove(0)
}

pub(crate) fn haiku() -> Skill {
    Skill::new("haiku", "Write a haiku.", "Three lines: 5, 7, 5.").unwrap()
}

pub(crate) fn sonnet() -> Skill {
    Skill::new("sonnet", "Write a sonnet.", "Fourteen lines.")
        .unwrap()
        .file("references/forms.md", "Petrarchan or Shakespearean.")
        .unwrap()
}

/// The names of the tools a request declared.
pub(crate) fn tool_names(request: &Request) -> Vec<&str> {
    request
        .tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect()
}
