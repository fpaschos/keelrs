//! The actor behind an [`AgentInstance`](super::AgentInstance): the only
//! writer of its instance's event stream while it holds the stream's lease.
//!
//! Each start takes the lease and recovers the stream; the actor then runs
//! dispatches one at a time and stops on any lease or store failure. The
//! library restarts it in place on the next command.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::oneshot;

use super::fold::Folded;
use super::{Command, SettledValue};
use crate::actors::lease::Lease;
use crate::actors::{Actor, Context, Stop};
use crate::agent::TurnSetup;
use crate::agent::rig::{self, Connect, Message, Run, Step};
use crate::builtin::Caches;
use crate::builtin::structured::{Structured, StructuredSpec};
use crate::event::Event;
use crate::store::{Epoch, Seq, Store, StoreError};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// What an aborted dispatch's unanswered tool calls are answered with.
const ABORTED: &str = "aborted";

/// What the actor is built from: instance `id` of the agent named `name`.
pub(super) struct Start {
    pub(super) name: String,
    pub(super) id: InstanceId,
    pub(super) setup: TurnSetup,
    /// The data the instance is created with, as JSON.
    pub(super) data: Arc<Value>,
    /// Connects the model each run declares.
    pub(super) connect: Connect,
    pub(super) store: Store,
}

/// Runs one agent instance's dispatches, one at a time, writing its events.
pub(super) struct InstanceActor {
    start: Arc<Start>,
    /// The held lease's epoch, which every append carries.
    epoch: Epoch,
    /// Whether a step is piped and not yet back.
    stepping: bool,
    /// Reads parked until their dispatch settles.
    waiting: HashMap<DispatchId, Vec<oneshot::Sender<Result<SettledValue>>>>,
    /// What this instance's built-in tools remember of its current sandbox; never stored.
    caches: Caches,
    folded: Folded,
}

impl Actor for InstanceActor {
    type Command = Command;
    type Args = Arc<Start>;

    /// Takes the lease, recovers the stream and starts its first dispatch.
    async fn start(start: &Arc<Start>, cx: &mut Context<Command>) -> Result<Self> {
        let lease =
            Lease::acquire(start.store.clone(), start.name.clone(), start.id.clone()).await?;
        let mut actor = Self {
            start: Arc::clone(start),
            epoch: lease.epoch(),
            stepping: false,
            waiting: HashMap::new(),
            caches: Caches::default(),
            folded: Folded::default(),
        };
        cx.hold(lease);
        actor.recover().await?;
        actor.start_next(cx);
        Ok(actor)
    }

    async fn handle(&mut self, command: Command, cx: &mut Context<Command>) -> Result<()> {
        match command {
            Command::Dispatch {
                dispatch_id,
                text,
                structured,
                respond_to,
            } => {
                if let Err(cause) = self.admit(dispatch_id, &text, structured.as_ref()).await {
                    cx.stash(Command::Dispatch {
                        dispatch_id,
                        text,
                        structured,
                        respond_to,
                    });
                    return Err(cause.into());
                }
                let _ = respond_to.send(Ok(()));
                self.start_next(cx);
            }
            Command::Read {
                dispatch_id,
                respond_to,
            } => {
                if let Some(result) = self.folded.settled.get(&dispatch_id) {
                    let _ = respond_to.send(answer(dispatch_id, result));
                } else if self
                    .folded
                    .pending
                    .iter()
                    .any(|a| a.dispatch == dispatch_id)
                {
                    self.waiting
                        .entry(dispatch_id)
                        .or_default()
                        .push(respond_to);
                } else {
                    let _ = respond_to.send(Err(Error::DispatchNotFound(dispatch_id)));
                }
            }
            Command::AbortQueued { respond_to } => {
                if let Err(cause) = self.abort(cx).await {
                    cx.stash(Command::AbortQueued { respond_to });
                    return Err(cause.into());
                }
                let _ = respond_to.send(Ok(()));
            }
            Command::Stepped {
                dispatch,
                run,
                step,
            } => self.stepped(dispatch, *run, step, cx).await?,
        }
        Ok(())
    }

    /// Stashes the parked reads: a graceful stop serves them at the next
    /// start, a failure fails them.
    async fn stopped(self, _: &Stop, cx: &mut Context<Command>) {
        for (dispatch_id, reads) in self.waiting {
            for respond_to in reads {
                cx.stash(Command::Read {
                    dispatch_id,
                    respond_to,
                });
            }
        }
    }
}

impl InstanceActor {
    /// Stores a dispatch, after the instance's `created` when the stream is empty.
    async fn admit(
        &mut self,
        dispatch_id: DispatchId,
        text: &str,
        structured: Option<&StructuredSpec>,
    ) -> Result<(), StoreError> {
        let mut events = Vec::new();
        if self.folded.last == Seq::default() {
            let data = Value::clone(&self.start.data);
            events.push((None, Event::Created { data }));
        }
        let (text, structured) = (text.to_owned(), structured.cloned());
        events.push((Some(dispatch_id), Event::Dispatched { text, structured }));
        self.append(events).await
    }

    /// Folds the stream from its first event, then settles a dispatch that
    /// was running when the last writer stopped.
    async fn recover(&mut self) -> Result<()> {
        let Start {
            name, id, store, ..
        } = &*self.start;
        for stored in store.read(name, id, Seq::default()).await? {
            let (seq, dispatch) = (stored.seq, stored.dispatch);
            let event = Event::of(stored).map_err(|reason| StoreError::Failed { reason })?;
            self.folded.apply(seq, dispatch, event);
        }
        if self
            .folded
            .data
            .as_ref()
            .is_some_and(|data| *data != *self.start.data)
        {
            return Err(Error::InstanceDataConflict {
                agent: self.start.name.clone(),
                id: self.start.id.clone(),
            });
        }
        // The recovery seam: Restart replaces this with continuing the dispatch.
        if let Some(first) = self.folded.pending.front()
            && first.started
        {
            let dispatch = first.dispatch;
            self.settle(dispatch, Vec::new(), Err(Outcome::Aborted))
                .await?;
        }
        Ok(())
    }

    /// Starts the first stored dispatch, unless one is running.
    fn start_next(&mut self, cx: &mut Context<Command>) {
        if self.stepping {
            return;
        }
        let Some(next) = self.folded.pending.front().cloned() else {
            return;
        };
        let run = Run::new(
            self.start.setup.clone(),
            next.text,
            self.folded.history.clone(),
            next.structured.map(Structured::from_spec),
            std::mem::take(&mut self.caches),
            self.start.connect.clone(),
        );
        self.step(next.dispatch, run, cx);
    }

    /// Takes `run`'s next step for `dispatch`; it comes back as
    /// [`Command::Stepped`].
    fn step(&mut self, dispatch: DispatchId, run: Run, cx: &mut Context<Command>) {
        self.stepping = true;
        cx.pipe_to_self(async move {
            let (run, step) = run.step().await;
            Command::Stepped {
                dispatch,
                run: Box::new(run),
                step,
            }
        });
    }

    /// Stores what `dispatch`'s step produced, then takes the next one,
    /// unless the lease may have run out meanwhile.
    async fn stepped(
        &mut self,
        dispatch: DispatchId,
        run: Run,
        step: Result<Step, String>,
        cx: &mut Context<Command>,
    ) -> Result<(), StoreError> {
        self.stepping = false;
        match step {
            Ok(Step::Append(events)) => {
                let events = events.into_iter().map(|e| (Some(dispatch), e)).collect();
                self.append(events).await?;
                cx.guard()?;
                self.step(dispatch, run, cx);
                return Ok(());
            }
            Ok(Step::Settled(events, value)) => {
                self.caches = run.into_caches();
                self.settle(dispatch, events, Ok(value)).await?;
            }
            Err(reason) => {
                self.caches = run.into_caches();
                self.settle(dispatch, Vec::new(), Err(Outcome::Failed { reason }))
                    .await?;
            }
        }
        self.start_next(cx);
        Ok(())
    }

    /// Stores `events` and how `dispatch` settled in one append, answering
    /// unanswered tool calls first when it did not finish.
    async fn settle(
        &mut self,
        dispatch: DispatchId,
        events: Vec<Event>,
        result: Result<SettledValue, Outcome>,
    ) -> Result<(), StoreError> {
        let mut events: Vec<_> = events.into_iter().map(|e| (Some(dispatch), e)).collect();
        if result.is_err() {
            events.extend(closing(&self.folded.history, dispatch));
        }
        events.push((Some(dispatch), Event::Settled { result }));
        self.append(events).await
    }

    /// Stops the running step and settles every queued dispatch as aborted.
    async fn abort(&mut self, cx: &mut Context<Command>) -> Result<(), StoreError> {
        if self.folded.pending.is_empty() {
            return Ok(());
        }
        if self.stepping {
            cx.unpipe();
            self.stepping = false;
            // The run held them; what they remembered of the sandbox is lost.
            self.caches = Caches::default();
        }
        let mut events = closing(&self.folded.history, self.folded.pending[0].dispatch);
        for admitted in &self.folded.pending {
            let result = Err(Outcome::Aborted);
            events.push((Some(admitted.dispatch), Event::Settled { result }));
        }
        self.append(events).await
    }

    /// Stores `events` after the stream's last, then folds them and answers
    /// the reads they settle.
    async fn append(&mut self, events: Vec<(Option<DispatchId>, Event)>) -> Result<(), StoreError> {
        let Start {
            name, id, store, ..
        } = &*self.start;
        let expected = self.folded.last;
        store
            .append(name, id, self.epoch, expected, &events)
            .await?;
        // Numbered as the store numbered them: `expected`+1…
        let mut seq = expected;
        for (dispatch, event) in events {
            seq = seq.next();
            let settles = matches!(event, Event::Settled { .. });
            self.folded.apply(seq, dispatch, event);
            if settles
                && let Some(dispatch) = dispatch
                && let Some(result) = self.folded.settled.get(&dispatch)
            {
                for read in self.waiting.remove(&dispatch).unwrap_or_default() {
                    let _ = read.send(answer(dispatch, result));
                }
            }
        }
        Ok(())
    }
}

/// An "aborted" result for each tool call `history` leaves unanswered.
fn closing(history: &[Message], dispatch: DispatchId) -> Vec<(Option<DispatchId>, Event)> {
    rig::close_pending(history, ABORTED)
        .into_iter()
        .map(|part| (Some(dispatch), Event::ToolResult(part)))
        .collect()
}

/// What a read of `dispatch_id` gets once it settled with `result`.
fn answer(dispatch_id: DispatchId, result: &Result<SettledValue, Outcome>) -> Result<SettledValue> {
    result.clone().map_err(|outcome| Error::Run {
        outcome,
        dispatch_id,
    })
}
