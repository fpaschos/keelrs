//! keelrs's canonical events: the source of truth for an agent instance (ADR 0004).
//!
//! Private and minimal for now: just enough to rebuild the history for the next
//! dispatch and to answer reads, once durable storage keeps them (`STORE:`).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::SettledValue;
use crate::agent::rig::Message;
use crate::{DispatchId, InstanceId, Outcome};

/// The name of one event: unique everywhere, minted by the writer before the append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct EventId(uuid::Uuid);

/// An event's position in its stream: 1, 2, 3… with no gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Seq(u64);

/// One event as its stream holds it.
#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "STORE: read back once durable storage keeps events"
)]
pub(crate) struct StoredEvent {
    pub(crate) id: EventId,
    pub(crate) agent: String,
    pub(crate) instance: InstanceId,
    pub(crate) seq: Seq,
    /// The dispatch that caused it; `None` outside a dispatch.
    pub(crate) dispatch: Option<DispatchId>,
    /// When the writer made it, ms since the Unix epoch.
    pub(crate) at: u64,
    pub(crate) event: Event,
}

/// One agent instance's event stream, as its single writer (the instance
/// actor) numbers it.
#[derive(Debug)]
pub(crate) struct Stream {
    agent: String,
    instance: InstanceId,
    /// The `seq` of the last event appended; 0 before the first.
    last: Seq,
}

impl Stream {
    /// The stream of instance `instance` of the agent named `agent`, empty.
    pub(crate) fn new(agent: String, instance: InstanceId) -> Self {
        Self {
            agent,
            instance,
            last: Seq(0),
        }
    }

    /// Wraps the events dispatch `dispatch` caused, numbered on from the last.
    pub(crate) fn append(&mut self, dispatch: DispatchId, events: Vec<Event>) -> Vec<StoredEvent> {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        events
            .into_iter()
            .map(|event| {
                self.last = Seq(self.last.0 + 1);
                StoredEvent {
                    id: EventId(uuid::Uuid::now_v7()),
                    agent: self.agent.clone(),
                    instance: self.instance.clone(),
                    seq: self.last,
                    dispatch: Some(dispatch),
                    at,
                    event,
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "STORE: read back once durable storage keeps events"
)]
pub(crate) enum Event {
    /// A message the run accepted (the user's prompt or the assistant's turn).
    Message(Message),
    /// The dispatched message settled: its reply text and structured result, or how it failed.
    Settled {
        dispatch_id: DispatchId,
        result: Result<SettledValue, Outcome>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstanceId;

    fn settled(dispatch_id: DispatchId) -> Event {
        Event::Settled {
            dispatch_id,
            result: Err(Outcome::Aborted),
        }
    }

    #[test]
    fn a_dispatchs_events_are_numbered_on_from_the_last() {
        let mut stream = Stream::new("agent".to_owned(), InstanceId::new("a"));
        let first = DispatchId::fresh();
        let stored = stream.append(first, vec![settled(first), settled(first), settled(first)]);
        assert_eq!(
            stored.iter().map(|e| e.seq.0).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(stored.iter().all(|e| e.dispatch == Some(first)));
        assert!(
            stored
                .iter()
                .all(|e| e.agent == "agent" && e.instance.as_str() == "a")
        );

        let second = DispatchId::fresh();
        let stored = stream.append(second, vec![settled(second)]);
        assert_eq!(stored.iter().map(|e| e.seq.0).collect::<Vec<_>>(), [4]);
        assert_eq!(stored[0].dispatch, Some(second));
    }
}
