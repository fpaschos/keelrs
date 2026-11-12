//! What an instance's event stream adds up to: [`Folded`], built by
//! [`Folded::apply`] alike when recovering the stream and after each append.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use super::SettledValue;
use crate::agent::rig::{self, Message};
use crate::builtin::structured::StructuredSpec;
use crate::event::Event;
use crate::store::Seq;
use crate::{DispatchId, Outcome};

/// A dispatch stored and not yet settled.
#[derive(Debug, Clone)]
pub(super) struct Admitted {
    pub(super) dispatch: DispatchId,
    pub(super) text: String,
    pub(super) structured: Option<StructuredSpec>,
    /// Whether its run has stored anything yet.
    pub(super) started: bool,
}

/// What the instance's event stream adds up to; changed only by `apply`.
#[derive(Debug, Default)]
pub(super) struct Folded {
    /// The `seq` of the last event.
    pub(super) last: Seq,
    /// The data the instance was created with.
    pub(super) data: Option<Value>,
    /// Every message of every dispatch, as the next run's history.
    pub(super) history: Vec<Message>,
    pub(super) settled: HashMap<DispatchId, Result<SettledValue, Outcome>>,
    /// Stored dispatches not yet settled, in order; the first may be running.
    pub(super) pending: VecDeque<Admitted>,
}

impl Folded {
    pub(super) fn apply(&mut self, seq: Seq, dispatch: Option<DispatchId>, event: Event) {
        self.last = seq;
        // Idempotent: the front dispatch's first message or tool result sets
        // `started`; every later one sets it again, which changes nothing.
        if matches!(event, Event::Message(_) | Event::ToolResult(_))
            && let Some(first) = self.pending.front_mut()
            && Some(first.dispatch) == dispatch
        {
            first.started = true;
        }
        match event {
            Event::Created { data } => self.data = Some(data),
            Event::Dispatched { text, structured } => {
                if let Some(dispatch) = dispatch {
                    self.pending.push_back(Admitted {
                        dispatch,
                        text,
                        structured,
                        started: false,
                    });
                }
            }
            Event::Message(message) => self.history.push(message),
            Event::ToolResult(part) => rig::push_result(&mut self.history, &part),
            Event::Settled { result } => {
                if let Some(dispatch) = dispatch {
                    self.pending.retain(|a| a.dispatch != dispatch);
                    self.settled.insert(dispatch, result);
                }
            }
        }
    }
}
