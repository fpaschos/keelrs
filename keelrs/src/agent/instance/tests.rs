//! What the instance's stream adds up to, fed by runs on rig-core's scripted
//! model: the history a later dispatch gets, and an abort's closing results.
//!
//! In-crate: no public seam gives a `Runtime` a scripted model, so these drive
//! [`Run`] and fold its events with [`Folded::apply`] as the actor does.

use rig_core::completion::CompletionRequest;
use rig_core::message::{AssistantContent, Message as RigMessage, ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};

use super::*;
use crate::agent::rig::tests::{agent as scripted, clock};
use crate::{Agent, AgentContext};

/// The turn setup of an agent on the mock model with one tool, `current_time`.
fn setup() -> TurnSetup {
    let agent = scripted(|cx| {
        cx.use_tool(clock());
    });
    TurnSetup::new(&agent, InstanceId::new("test"), ())
}

/// A run of `text` after the folded history, against a model replying with `turns`.
fn run(
    folded: &Folded,
    text: &str,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Run, MockCompletionModel) {
    let model = MockCompletionModel::from_turns(turns);
    let mock = model.clone();
    let run = Run::with_connect(
        setup(),
        text.to_owned(),
        folded.history.clone(),
        None,
        Caches::default(),
        Box::new(move |_| Ok(mock.clone().erase())),
    );
    (run, model)
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

    /// Steps `run` to settled, folding every step.
    async fn settle(&mut self, dispatch: DispatchId, mut run: Run) {
        loop {
            let (next, step) = run.step().await;
            run = next;
            match step.unwrap() {
                Step::Append(events) => self.fold(dispatch, events),
                Step::Settled(events, value) => {
                    self.fold(dispatch, events);
                    let result = Ok(value);
                    self.fold(dispatch, [Event::Settled { result }]);
                    return;
                }
            }
        }
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
    let stored: Vec<StoredEvent> = events
        .iter()
        .zip(1..)
        .map(|((dispatch, event), seq)| {
            let (kind, event) = event.to_parts();
            StoredEvent {
                id: EventId::fresh(),
                agent: agent.to_owned(),
                instance: id.clone(),
                seq: Seq::new(seq),
                dispatch: *dispatch,
                at: 0,
                kind,
                event,
            }
        })
        .collect();
    let epoch = store.acquire(agent, id).await.unwrap();
    store
        .append(agent, id, epoch, Seq::default(), &stored)
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
