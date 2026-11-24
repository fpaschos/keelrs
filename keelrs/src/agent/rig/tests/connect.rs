//! How often a run connects its model, counted on a `Connect` of its own:
//! no public seam sees a connection.

use std::sync::atomic::AtomicUsize;

use rig_core::test_utils::{MockCompletionModel, MockTurn};

use super::*;
use crate::agent::TurnSetup;
use crate::builtin::Caches;
use crate::event::Event;
use crate::{Agent, AgentContext, InstanceId};

#[tokio::test]
async fn turns_that_declare_the_same_model_connect_it_once() {
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("c1", "lookup_order", json!({"order_id": "A1"})),
        MockTurn::text("Found it."),
    ]);
    let agent = Agent::new("scripted", |cx: &mut AgentContext| {
        cx.use_model("mock/model");
        cx.use_sandbox(&unused()).only([]);
        cx.use_tool(lookup_order());
        String::new()
    });
    let connections = Arc::new(AtomicUsize::new(0));
    let (mock, counted) = (model.clone(), connections.clone());
    let run = Run::new(
        TurnSetup::new(&agent, InstanceId::new("test"), ()),
        "Go".to_owned(),
        Vec::new(),
        None,
        Caches::default(),
        Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(mock.clone().erase())
        }),
    );
    let mut stored: Vec<Event> = Vec::new();
    let completed = run.drive(&mut stored).await;
    assert_eq!(completed.result.expect("the run settled").text, "Found it.");
    assert_eq!(model.requests().len(), 2);
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}
