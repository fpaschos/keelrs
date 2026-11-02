//! Dispatch, read and ask through the public API, with no network.

mod common;

use common::temp_sandbox;
use std::sync::{Arc, Mutex};

use keelrs::{Agent, AgentContext, Error, InstanceId, Outcome, Runtime};
use serde::{Deserialize, Serialize};

/// Asks a fresh instance of `agent` one message and returns its failure reason.
async fn failure_reason(agent: Agent) -> String {
    let runtime = Runtime::start([&agent]).unwrap();
    let instance = runtime.instance(&agent, InstanceId::new("test")).unwrap();
    match instance.ask("Say hello").await {
        Err(Error::Run {
            outcome: Outcome::Failed { reason },
            ..
        }) => reason,
        other => panic!("expected a failed run, got {other:?}"),
    }
}

#[tokio::test]
async fn an_agent_that_never_calls_use_model_fails() {
    let agent = Agent::new("no-model", |_: &mut AgentContext| "Greet.".into());
    let reason = failure_reason(agent).await;
    assert!(reason.contains("never called `use_model`"), "{reason}");
}

#[tokio::test]
async fn an_agent_that_calls_use_model_twice_fails() {
    let agent = Agent::new("two-models", |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_model("anthropic/claude-opus-5-5");
        "Greet.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(reason.contains("called 2 times"), "{reason}");
}

#[tokio::test]
async fn an_empty_model_id_fails() {
    let agent = Agent::new("empty-model", |cx: &mut AgentContext| {
        cx.use_model("");
        "Greet.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(reason.contains("empty model id"), "{reason}");
}

#[tokio::test]
async fn blank_use_instruction_text_fails() {
    let sb = temp_sandbox().await;
    let agent = Agent::new("blank-instruction", move |cx: &mut AgentContext| {
        cx.use_model("anthropic/claude-sonnet-5-5");
        cx.use_sandbox(&sb).only([]);
        cx.use_instruction("  ");
        "Greet.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(
        reason.contains("`use_instruction` was given empty text"),
        "{reason}"
    );
}

#[tokio::test]
async fn an_unknown_vendor_fails_with_rigs_reason() {
    let sb = temp_sandbox().await;
    let agent = Agent::new("unknown-vendor", move |cx: &mut AgentContext| {
        cx.use_model("nope/model");
        cx.use_sandbox(&sb).only([]);
        "Greet.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(reason.contains("nope"), "{reason}");
}

#[tokio::test]
async fn a_read_issued_before_the_run_settles_gets_the_outcome() {
    let agent = Agent::new("no-model", |_: &mut AgentContext| "Greet.".into());
    let runtime = Runtime::start([&agent]).unwrap();
    let instance = runtime.instance(&agent, InstanceId::new("test")).unwrap();
    let first = instance.dispatch("one").await.unwrap();
    let second = instance.dispatch("two").await.unwrap();

    // `second` waits behind `first` (one run at a time), so this read arrives
    // while it is still pending and must wait for it to settle.
    for receipt in [&second, &first] {
        let result = instance.read(receipt).await;
        assert!(
            matches!(
                &result,
                Err(Error::Run { outcome: Outcome::Failed { .. }, submission_id })
                    if *submission_id == receipt.submission_id()
            ),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn reading_a_receipt_on_another_instance_fails() {
    let agent = Agent::new("no-model", |_: &mut AgentContext| "Greet.".into());
    let runtime = Runtime::start([&agent]).unwrap();
    let receipt = runtime
        .instance(&agent, InstanceId::new("a"))
        .unwrap()
        .dispatch("hi")
        .await
        .unwrap();
    let result = runtime
        .instance(&agent, InstanceId::new("b"))
        .unwrap()
        .read(&receipt)
        .await;
    assert!(matches!(result, Err(Error::SubmissionNotFound(id)) if id == receipt.submission_id()));
}

#[test]
fn fresh_gives_a_distinct_uuid_v7_each_time() {
    let (a, b) = (InstanceId::fresh(), InstanceId::fresh());
    assert_ne!(a, b);
    for id in [&a, &b] {
        assert_eq!(
            uuid::Uuid::parse_str(id.as_str())
                .unwrap()
                .get_version_num(),
            7,
            "{id}"
        );
    }
}

#[test]
fn an_instance_id_serializes_as_its_bare_string() {
    let id = InstanceId::new("implement-17");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""implement-17""#);
    assert_eq!(serde_json::from_str::<InstanceId>(&json).unwrap(), id);
}

// Instance data and ids. Each agent records what its render saw, then fails
// for want of a model: no network.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Issue {
    number: u64,
}

type Seen = Arc<Mutex<Vec<(String, u64)>>>;

/// An agent that records its instance's id and issue number on every render.
fn recorder(seen: &Seen) -> Agent<Issue> {
    let seen = seen.clone();
    Agent::new("recorder", move |cx: &mut AgentContext<Issue>| {
        let entry = (cx.id().to_string(), cx.data().number);
        seen.lock().unwrap().push(entry);
        String::new()
    })
}

#[tokio::test]
async fn a_render_reads_its_instances_data_and_id() {
    let seen = Seen::default();
    let agent = recorder(&seen);
    let runtime = Runtime::start([&agent]).unwrap();
    let a = runtime
        .instance_with(&agent, InstanceId::new("a"), Issue { number: 1 })
        .unwrap();
    let b = runtime
        .instance_with(&agent, InstanceId::new("b"), Issue { number: 2 })
        .unwrap();
    let _ = a.ask("go").await;
    let _ = b.ask("go").await;
    assert_eq!(
        *seen.lock().unwrap(),
        [("a".to_owned(), 1), ("b".to_owned(), 2)]
    );
}

#[tokio::test]
async fn an_existing_id_keeps_its_first_data() {
    let seen = Seen::default();
    let agent = recorder(&seen);
    let runtime = Runtime::start([&agent]).unwrap();
    let first = runtime
        .instance_with(&agent, InstanceId::new("a"), Issue { number: 1 })
        .unwrap();
    let _ = first.ask("go").await;
    let again = runtime
        .instance_with(&agent, InstanceId::new("a"), Issue { number: 2 })
        .unwrap();
    let _ = again.ask("go").await;
    assert_eq!(
        *seen.lock().unwrap(),
        [("a".to_owned(), 1), ("a".to_owned(), 1)]
    );
}

#[tokio::test]
async fn a_blank_id_fails() {
    let agent = Agent::new("no-model", |_: &mut AgentContext| String::new());
    let runtime = Runtime::start([&agent]).unwrap();
    for blank in ["", "  \t"] {
        let result = runtime.instance(&agent, InstanceId::new(blank));
        assert!(
            matches!(&result, Err(Error::InvalidInstanceId(id)) if id.as_str() == blank),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn an_unregistered_agent_fails() {
    let registered = Agent::new("registered", |_: &mut AgentContext| String::new());
    let other = Agent::new("other", |_: &mut AgentContext| String::new());
    let runtime = Runtime::start([&registered]).unwrap();
    let result = runtime.instance(&other, InstanceId::new("a"));
    assert!(
        matches!(&result, Err(Error::UnknownAgent(name)) if name == "other"),
        "{result:?}"
    );
}

#[tokio::test]
async fn another_agent_with_a_registered_name_fails() {
    let registered = Agent::new("twin", |_: &mut AgentContext| String::new());
    let impostor = Agent::new("twin", |_: &mut AgentContext| String::new());
    let runtime = Runtime::start([&registered]).unwrap();
    let result = runtime.instance(&impostor, InstanceId::new("a"));
    assert!(
        matches!(&result, Err(Error::UnknownAgent(name)) if name == "twin"),
        "{result:?}"
    );
}

#[test]
fn two_agents_with_one_name_fail_to_start() {
    let a = Agent::new("twin", |_: &mut AgentContext| String::new());
    let b = Agent::new("twin", |_: &mut AgentContext<Issue>| String::new());
    let result = Runtime::start((&a, &b));
    assert!(
        matches!(&result, Err(Error::DuplicateAgent(name)) if name == "twin"),
        "{result:?}"
    );
}

#[tokio::test]
async fn start_takes_a_tuple_an_array_and_a_vec() {
    let plain = Agent::new("plain", |_: &mut AgentContext| String::new());
    let seen = Seen::default();
    let with_data = recorder(&seen);
    let runtime = Runtime::start((&plain, &with_data)).unwrap();
    runtime.instance(&plain, InstanceId::new("a")).unwrap();
    runtime
        .instance_with(&with_data, InstanceId::new("a"), Issue { number: 1 })
        .unwrap();

    let agents: Vec<Agent<serde_json::Value>> = ["one", "two"]
        .into_iter()
        .map(|name| {
            Agent::new(name, |_: &mut AgentContext<serde_json::Value>| {
                String::new()
            })
        })
        .collect();
    let runtime = Runtime::start(&agents).unwrap();
    for agent in &agents {
        runtime
            .instance_with(agent, InstanceId::new("a"), serde_json::Value::Null)
            .unwrap();
    }
    let runtime = Runtime::start([&plain]).unwrap();
    runtime.instance(&plain, InstanceId::new("a")).unwrap();
    let runtime = Runtime::start(agents).unwrap();
    drop(runtime);
}
