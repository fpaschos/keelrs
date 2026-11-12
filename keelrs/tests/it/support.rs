//! What the tests share: temp dirs, sandboxes on them, and agents on a scripted model.

#![allow(clippy::disallowed_methods)] // the dir is made and removed on the host

use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use keelrs::sandbox::{self, Sandbox};
use keelrs::store::StoreError;
use keelrs::testing::{Request, ScriptedModel};
use keelrs::{Agent, AgentContext, Error, InstanceId, Outcome, Runtime, Skill, Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::time::timeout;

/// A fresh dir under the system temp dir, removed on drop.
pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    /// The path `relative` in the dir.
    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// Writes `contents` at `relative`, creating its folders.
    pub(crate) fn write(&self, relative: &str, contents: &str) -> &Self {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    /// What the dir holds at `relative`; `None` when nothing is there.
    pub(crate) fn read(&self, relative: &str) -> Option<String> {
        fs::read_to_string(self.path(relative)).ok()
    }

    /// Writes `<root>/<folder>/SKILL.md` naming the skill `name`.
    pub(crate) fn skill(&self, root: &str, folder: &str, name: &str, description: &str) -> &Self {
        self.write(
            &format!("{root}/{folder}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nBody of {name}.\n"),
        )
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A local sandbox rooted at `dir`.
pub(crate) async fn open(dir: &TempDir) -> Sandbox {
    sandbox::local().root(&dir.0).open().await.unwrap()
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

/// A runtime hosting `agent`, its turns answered by `model`.
pub(crate) fn runtime(agent: &Agent, model: &ScriptedModel) -> Runtime {
    Runtime::builder()
        .model(model.clone())
        .start([agent])
        .unwrap()
}

/// Asks a fresh instance of `agent` `message`, answered by `model`; the reply's text.
pub(crate) async fn scripted(
    agent: &Agent,
    model: &ScriptedModel,
    message: &str,
) -> keelrs::Result<String> {
    let runtime = runtime(agent, model);
    let instance = runtime.instance(agent, InstanceId::fresh()).unwrap();
    let reply = timeout(TIMEOUT, instance.ask(message)).await;
    reply
        .expect("the dispatch settles")
        .map(|reply| reply.text().to_owned())
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

/// Every tool result the model's last request carried, in order: `result:<text>`
/// or `error:<text>`.
pub(crate) fn last_results(model: &ScriptedModel) -> Vec<String> {
    let requests = model.requests();
    let last = requests.last().expect("the model was called");
    last.messages
        .iter()
        .filter(|m| m.starts_with("result:") || m.starts_with("error:"))
        .cloned()
        .collect()
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
