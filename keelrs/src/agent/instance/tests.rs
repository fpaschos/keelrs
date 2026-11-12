//! What the instance's stream adds up to, fed by runs on rig-core's scripted
//! model: the history a later dispatch gets, and an abort's closing results.
//!
//! In-crate: no public seam gives a `Runtime` a scripted model, so these drive
//! [`Run`] and fold its events with [`Folded::apply`] as the actor does.

use rig_core::completion::CompletionRequest;
use rig_core::message::{AssistantContent, Message as RigMessage, ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};

use std::time::Duration;

use super::actor::closing;
use super::fold::Folded;
use super::*;
use crate::agent::rig::tests::{agent as scripted, clock, scripted_run};
use crate::agent::rig::{Run, Step, tests as rig_tests};
use crate::builtin::Caches;
use crate::event::Event;
use crate::store::Seq;
use crate::{Agent, AgentContext};

/// A run of `text` after the folded history, by an agent with one tool,
/// `current_time`, against a model replying with `turns`.
fn run(
    folded: &Folded,
    text: &str,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Run, MockCompletionModel) {
    let agent = scripted(|cx| {
        cx.use_tool(clock());
    });
    let history = folded.history.clone();
    scripted_run(&agent, text, history, None, Caches::default(), turns)
}

/// Test helpers that fold as the actor appends, numbering each event.
impl Folded {
    fn fold(&mut self, dispatch: DispatchId, events: impl IntoIterator<Item = Event>) {
        for event in events {
            self.apply(self.last.next(), Some(dispatch), event);
        }
    }

    fn dispatch(&mut self, text: &str) -> DispatchId {
        let dispatch = DispatchId::fresh();
        let text = text.to_owned();
        let structured = None;
        self.fold(dispatch, [Event::Dispatched { text, structured }]);
        dispatch
    }

    /// Runs `run` to settled and folds what it accepted, then `settled`.
    async fn settle(&mut self, dispatch: DispatchId, run: Run) {
        let settled = rig_tests::settle(run).await.1.expect("the run settles");
        self.fold(dispatch, settled.messages.into_iter().map(Event::Message));
        let text = settled.reply;
        let result = Ok(SettledValue {
            text,
            structured: settled.structured,
        });
        self.fold(dispatch, [Event::Settled { result }]);
    }
}

/// Each message of the request's history (the prompt last), as its text, tool calls
/// as `call:<name>` and results as `result:<text>`.
fn sent(request: &CompletionRequest) -> Vec<String> {
    request
        .chat_history
        .iter()
        .map(AsRef::as_ref)
        .flat_map(|message| match message {
            RigMessage::User { content } => content
                .iter()
                .map(|c| match c {
                    UserContent::Text(text) => text.text.clone(),
                    UserContent::ToolResult(result) => {
                        let text: String = result
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                ToolResultContent::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect();
                        format!("result:{text}")
                    }
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>(),
            RigMessage::Assistant(reply) => reply
                .content
                .iter()
                .map(|c| match c {
                    AssistantContent::Text(text) => text.text.clone(),
                    AssistantContent::ToolCall(call) => format!("call:{}", call.function.name),
                    other => format!("{other:?}"),
                })
                .collect(),
            RigMessage::System { .. } => Vec::new(),
        })
        .collect()
}

#[tokio::test]
async fn a_second_dispatch_runs_after_the_first_ones_messages() {
    let mut stream = Folded::default();
    let first = stream.dispatch("What time is it?");
    let (run1, _) = run(
        &stream,
        "What time is it?",
        [
            MockTurn::tool_call("c1", "current_time", serde_json::json!({})),
            MockTurn::text("It is noon."),
        ],
    );
    stream.settle(first, run1).await;
    assert!(stream.pending.is_empty());
    assert!(matches!(stream.settled.get(&first), Some(Ok(_))));

    let second = stream.dispatch("And now?");
    let (run2, model) = run(&stream, "And now?", [MockTurn::text("Still noon.")]);
    stream.settle(second, run2).await;
    assert_eq!(
        sent(&model.requests()[0]),
        [
            "What time is it?",
            "call:current_time",
            "result:noon",
            "It is noon.",
            "And now?"
        ]
    );
}

#[tokio::test]
async fn an_aborted_dispatchs_open_tool_calls_are_answered_before_the_next_one() {
    let mut stream = Folded::default();
    let aborted = stream.dispatch("What time is it?");
    let (run1, _) = run(
        &stream,
        "What time is it?",
        [MockTurn::tool_call(
            "c1",
            "current_time",
            serde_json::json!({}),
        )],
    );
    // The model answered with a call; the abort comes before the tool runs.
    let (_, step) = run1.step().await;
    let Ok(Step::Append(events)) = step else {
        panic!("expected the model's answer, got {step:?}");
    };
    stream.fold(aborted, events);
    assert!(stream.pending[0].started);

    let closing = closing(&stream.history, aborted);
    stream.fold(aborted, closing.into_iter().map(|(_, event)| event));
    let result = Err(Outcome::Aborted);
    stream.fold(aborted, [Event::Settled { result }]);
    assert_eq!(stream.settled.get(&aborted), Some(&Err(Outcome::Aborted)));

    let next = stream.dispatch("Try again.");
    let (run2, model) = run(&stream, "Try again.", [MockTurn::text("Noon.")]);
    stream.settle(next, run2).await;
    assert_eq!(
        sent(&model.requests()[0]),
        [
            "What time is it?",
            "call:current_time",
            "result:aborted",
            "Try again."
        ]
    );
}

/// A stream another writer left with `created` and two dispatches that never
/// ran, its lease released; and those dispatches' receipts.
async fn left_queued(store: &Store, agent: &str, id: &InstanceId) -> [Receipt; 2] {
    let receipts = [DispatchId::fresh(), DispatchId::fresh()];
    let events = [
        (None, Event::Created { data: Value::Null }),
        (
            Some(receipts[0]),
            Event::Dispatched {
                text: "One".to_owned(),
                structured: None,
            },
        ),
        (
            Some(receipts[1]),
            Event::Dispatched {
                text: "Two".to_owned(),
                structured: None,
            },
        ),
    ];
    let epoch = store.acquire(agent, id).await.unwrap();
    store
        .append(agent, id, epoch, Seq::default(), &events)
        .await
        .unwrap();
    store.release(agent, id, epoch).await.unwrap();
    receipts.map(|dispatch_id| Receipt { dispatch_id })
}

/// A handle to a fresh actor over `store` for an agent whose runs fail at once.
fn spawned(store: &Store, id: &InstanceId) -> AgentInstance {
    let agent = Agent::new("queued", |_: &mut AgentContext| String::new());
    AgentInstance::spawn(
        "queued".to_owned(),
        TurnSetup::new(&agent, id.clone(), ()),
        id.clone(),
        Value::Null,
        store.clone(),
        &CancellationToken::new(),
    )
}

#[tokio::test]
async fn a_read_that_recovers_the_stream_runs_its_queued_dispatches() {
    let store = Store::in_memory();
    let id = InstanceId::new("queued");
    let [_, second] = left_queued(&store, "queued", &id).await;
    let instance = spawned(&store, &id);
    let read = tokio::time::timeout(Duration::from_secs(5), instance.read(&second)).await;
    assert!(
        matches!(
            read,
            Ok(Err(Error::Run {
                outcome: Outcome::Failed { .. },
                ..
            }))
        ),
        "got {read:?}"
    );
}

#[tokio::test]
async fn abort_settles_dispatches_it_has_not_recovered_yet() {
    let store = Store::in_memory();
    let id = InstanceId::new("queued");
    let receipts = left_queued(&store, "queued", &id).await;
    let instance = spawned(&store, &id);
    instance.abort().await.unwrap();
    for receipt in &receipts {
        let read = tokio::time::timeout(Duration::from_secs(5), instance.read(receipt)).await;
        assert!(
            matches!(
                read,
                Ok(Err(Error::Run {
                    outcome: Outcome::Aborted,
                    ..
                }))
            ),
            "got {read:?}"
        );
    }
}
