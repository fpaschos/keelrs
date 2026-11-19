//! A structured run's failed model turn, driven through `Run` on rig-core's
//! scripted model: `ScriptedModel` cannot end a turn by its length, so this
//! needs the private `Run` and a `Connect` of its own.

use rig_core::completion::FinishReason;
use rig_core::test_utils::{MockCompletionModel, MockTurn};

use super::*;
use crate::agent::TurnSetup;
use crate::builtin::Caches;
use crate::builtin::structured::{FINISH_STRUCTURED, Structured, StructuredSpec};
use crate::event::Event;
use crate::{Agent, AgentContext, InstanceId};

/// A plan of work.
#[derive(Deserialize, JsonSchema)]
struct Plan {
    #[expect(dead_code, reason = "only the schema is used")]
    steps: Vec<String>,
}

#[tokio::test]
async fn a_failed_model_turn_is_not_re_asked_as_a_missing_result() {
    let model = MockCompletionModel::from_turns([
        MockTurn::text("").with_finish_reason(FinishReason::Length),
        MockTurn::tool_call("c1", FINISH_STRUCTURED, json!({"steps": []})),
    ]);
    let agent = Agent::new("scripted", |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        cx.use_sandbox(&unused()).only([]);
        String::new()
    });
    let mock = model.clone();
    let run = Run::new(
        TurnSetup::new(&agent, InstanceId::new("test"), ()),
        "Go".to_owned(),
        Vec::new(),
        Some(Structured::from_spec(StructuredSpec::of::<Plan>())),
        Caches::default(),
        Arc::new(move |_| Ok(mock.clone().erase())),
    );
    let mut stored = Vec::new();
    let completed = run.drive(&mut stored).await;
    let reason = completed.result.expect_err("the run settled");
    assert!(stored.is_empty(), "the failed turn stored {stored:?}");
    assert!(reason.contains("max_tokens"), "{reason}");
    assert_eq!(model.requests().len(), 1);
}

/// A sink that keeps what the run persisted.
impl PersistSink for &mut Vec<Event> {
    async fn persist(&mut self, events: Vec<Event>) {
        self.extend(events);
    }
}
