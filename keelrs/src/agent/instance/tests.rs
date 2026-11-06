//! What the instance's stream adds up to, fed by runs on rig-core's scripted
//! model: the history a later dispatch gets, and an abort's closing results.
//!
//! In-crate: no public seam gives a `Runtime` a scripted model, so these drive
//! [`Run`] and fold its events with [`Folded::apply`] as the actor does.

use std::convert::Infallible;

use rig_core::completion::CompletionRequest;
use rig_core::message::{AssistantContent, Message as RigMessage, ToolResultContent, UserContent};
use rig_core::test_utils::{MockCompletionModel, MockTurn};

use super::*;
use crate::sandbox::unused;
use crate::{Agent, AgentContext, Tool, ToolCall};

/// An agent on the mock model with one tool, `current_time`.
fn agent() -> InstanceAgent {
    let sb = unused();
    Agent::new("scripted", move |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        cx.use_sandbox(&sb).only([]);
        cx.use_tool(Tool::host(
            "current_time",
            "The current time.",
            async |_: ToolCall<()>| Ok::<_, Infallible>("noon"),
        ));
        String::new()
    })
    .for_instance(InstanceId::new("test"), ())
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
        agent(),
        text.to_owned(),
        folded.history.clone(),
        None,
        Caches::default(),
        Box::new(move |_| Ok(mock.clone().erase())),
    );
    (run, model)
}

/// A stream fold with seq numbering, as the actor appends.
struct Stream {
    folded: Folded,
}

impl Stream {
    fn new() -> Self {
        Self {
            folded: Folded::default(),
        }
    }

    fn apply(&mut self, dispatch: DispatchId, events: impl IntoIterator<Item = Event>) {
        for event in events {
            let seq = self.folded.last.next();
            self.folded.apply(seq, Some(dispatch), &event);
        }
    }

    fn dispatch(&mut self, text: &str) -> DispatchId {
        let dispatch = DispatchId::fresh();
        let text = text.to_owned();
        let structured = None;
        self.apply(dispatch, [Event::Dispatched { text, structured }]);
        dispatch
    }

    /// Steps `run` to settled, folding every step.
    async fn settle(&mut self, dispatch: DispatchId, mut run: Run) {
        loop {
            let (next, step) = run.step().await;
            run = next;
            match step.unwrap() {
                Step::Append(events) => self.apply(dispatch, events),
                Step::Settled(events, value) => {
                    self.apply(dispatch, events);
                    let result = Ok(value);
                    self.apply(dispatch, [Event::Settled { result }]);
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
    let mut stream = Stream::new();
    let first = stream.dispatch("What time is it?");
    let (run1, _) = run(
        &stream.folded,
        "What time is it?",
        [
            MockTurn::tool_call("c1", "current_time", serde_json::json!({})),
            MockTurn::text("It is noon."),
        ],
    );
    stream.settle(first, run1).await;
    assert!(stream.folded.pending.is_empty());
    assert!(matches!(stream.folded.settled.get(&first), Some(Ok(_))));

    let second = stream.dispatch("And now?");
    let (run2, model) = run(&stream.folded, "And now?", [MockTurn::text("Still noon.")]);
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
    let mut stream = Stream::new();
    let aborted = stream.dispatch("What time is it?");
    let (run1, _) = run(
        &stream.folded,
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
    stream.apply(aborted, events);
    assert!(stream.folded.pending[0].started);

    let closing = closing(&stream.folded.history, aborted);
    stream.apply(aborted, closing.into_iter().map(|(_, event)| event));
    let result = Err(Outcome::Aborted);
    stream.apply(aborted, [Event::Settled { result }]);
    assert_eq!(
        stream.folded.settled.get(&aborted),
        Some(&Err(Outcome::Aborted))
    );

    let next = stream.dispatch("Try again.");
    let (run2, model) = run(&stream.folded, "Try again.", [MockTurn::text("Noon.")]);
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
