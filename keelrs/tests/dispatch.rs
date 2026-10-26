//! Dispatch and read through the public API, with no network.

use keelrs::{Agent, AgentContext, Error, Outcome, Runtime};

/// Dispatches one message to a fresh instance of `agent` and reads its failure reason.
async fn failure_reason(agent: Agent) -> String {
    let runtime = Runtime::start([agent.clone()]);
    let instance = runtime.init(&agent).id("test").unwrap();
    let receipt = instance.dispatch("Say hello").await.unwrap();
    match instance.read(&receipt).await {
        Err(Error::Run {
            outcome: Outcome::Failed { reason },
            submission_id,
        }) => {
            assert_eq!(submission_id, receipt.submission_id());
            reason
        }
        other => panic!("expected a failed run, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "STORE: reads need durable events"]
async fn an_agent_that_never_calls_use_model_fails() {
    let agent = Agent::new("no-model", |_: &mut AgentContext| "Greet.".into());
    let reason = failure_reason(agent).await;
    assert!(reason.contains("never called `use_model`"), "{reason}");
}

#[tokio::test]
#[ignore = "STORE: reads need durable events"]
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
#[ignore = "STORE: reads need durable events"]
async fn an_empty_model_id_fails() {
    let agent = Agent::new("empty-model", |cx: &mut AgentContext| {
        cx.use_model("");
        "Greet.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(reason.contains("empty model id"), "{reason}");
}

#[tokio::test]
#[ignore = "STORE: reads need durable events"]
async fn an_unknown_vendor_fails_with_rigs_reason() {
    let agent = Agent::new("unknown-vendor", |cx: &mut AgentContext| {
        cx.use_model("nope/model");
        "Greet.".into()
    });
    let reason = failure_reason(agent).await;
    assert!(reason.contains("nope"), "{reason}");
}

#[tokio::test]
#[ignore = "STORE: reads need durable events"]
async fn a_read_issued_before_the_run_settles_gets_the_outcome() {
    let agent = Agent::new("no-model", |_: &mut AgentContext| "Greet.".into());
    let runtime = Runtime::start([agent.clone()]);
    let instance = runtime.init(&agent).id("test").unwrap();
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
    let runtime = Runtime::start([agent.clone()]);
    let receipt = runtime
        .init(&agent)
        .id("a")
        .unwrap()
        .dispatch("hi")
        .await
        .unwrap();
    let result = runtime.init(&agent).id("b").unwrap().read(&receipt).await;
    assert!(matches!(result, Err(Error::SubmissionNotFound(id)) if id == receipt.submission_id()));
}
