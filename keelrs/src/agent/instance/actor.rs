//! The actor behind an [`AgentInstance`](super::AgentInstance): the only
//! writer of its instance's event stream while it holds the stream's lease.
//!
//! Its key's task runs one incarnation at a time, restarted in place: an
//! incarnation takes the lease and recovers the stream once (its start),
//! serves, and ends on a stop or on any lease or store failure, failing its
//! callers with the cause. The next command starts the next incarnation.

use std::collections::HashMap;

use serde_json::Value;
use tokio::sync::oneshot;

use super::fold::Folded;
use super::{Command, SettledValue};
use crate::actors::lease::Lease;
use crate::actors::{Mailbox, Next};
use crate::agent::TurnSetup;
use crate::agent::rig::{self, Connect, Message, Run, Step};
use crate::builtin::Caches;
use crate::builtin::structured::{Structured, StructuredSpec};
use crate::event::Event;
use crate::store::{Epoch, Seq, Store, StoreError};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// What an aborted dispatch's unanswered tool calls are answered with.
const ABORTED: &str = "aborted";

/// Answered once what was asked is stored.
type Stored = oneshot::Sender<Result<()>>;

/// Reads parked until their dispatch settles.
type Waiting = HashMap<DispatchId, Vec<oneshot::Sender<Result<SettledValue>>>>;

/// What the actor needs to start: instance `id` of the agent named `name`.
pub(super) struct Start {
    pub(super) name: String,
    pub(super) id: InstanceId,
    pub(super) setup: TurnSetup,
    /// The data the instance is created with, as JSON.
    pub(super) data: Value,
    /// Connects the model each run declares.
    pub(super) connect: Connect,
    pub(super) store: Store,
}

/// Why an incarnation ended.
enum End {
    /// The runtime was dropped.
    Shutdown,
    /// A stop: the inbox and the parked reads carry over.
    Stopped(Waiting),
    /// A lease or store failure, the call that failed, and the parked reads.
    Failed(Error, Option<Stored>, Waiting),
}

/// The key's task: lazy, and each pass one incarnation. Commands sent
/// during a start wait in the inbox, in order.
pub(super) async fn run(start: Start, mut mailbox: Mailbox<Command>) {
    let mut waiting = Waiting::new();
    loop {
        // No lease until the first command.
        let first = loop {
            match mailbox.next().await {
                None => return,
                Some(Next::Command(command)) => break command,
                // No incarnation runs: nothing to stop.
                Some(Next::Stopped | Next::Ended(_)) => {}
            }
        };
        let (end, unserved) = match Incarnation::start(&start, &mut mailbox).await {
            Ok(mut actor) => {
                actor.park(waiting);
                (actor.serve(first, &mut mailbox).await, None)
            }
            Err(cause) => (End::Failed(cause, None, waiting), Some(first)),
        };
        mailbox.end().await;
        waiting = match end {
            End::Shutdown => return,
            End::Stopped(waiting) => waiting,
            End::Failed(cause, call, reads) => {
                if let Some(call) = call {
                    let _ = call.send(Err(cause.clone()));
                }
                for read in reads.into_values().flatten() {
                    let _ = read.send(Err(cause.clone()));
                }
                for command in unserved.into_iter().chain(mailbox.drain()) {
                    command.fail(cause.clone());
                }
                Waiting::new()
            }
        };
    }
}

/// One incarnation: runs its instance's dispatches, one at a time, writing
/// its events.
struct Incarnation<'s> {
    start: &'s Start,
    /// The held lease's epoch, which every append carries.
    epoch: Epoch,
    /// Whether a step is piped and not yet back.
    stepping: bool,
    waiting: Waiting,
    /// What this instance's built-in tools remember of its current sandbox; never stored.
    caches: Caches,
    folded: Folded,
}

impl<'s> Incarnation<'s> {
    /// Takes the lease, recovers the stream and starts its first dispatch.
    async fn start(start: &'s Start, mailbox: &mut Mailbox<Command>) -> Result<Self> {
        let lease =
            Lease::acquire(start.store.clone(), start.name.clone(), start.id.clone()).await?;
        let mut actor = Self {
            start,
            epoch: lease.epoch(),
            stepping: false,
            waiting: Waiting::new(),
            caches: Caches::default(),
            folded: Folded::default(),
        };
        mailbox.hold(lease);
        actor.recover().await?;
        actor.start_next(mailbox);
        Ok(actor)
    }

    /// Parks `reads` carried over a stop, answering those already settled.
    fn park(&mut self, reads: Waiting) {
        for (dispatch, reads) in reads {
            match self.folded.settled.get(&dispatch) {
                Some(result) => {
                    for read in reads {
                        let _ = read.send(answer(dispatch, result));
                    }
                }
                None => self.waiting.entry(dispatch).or_default().extend(reads),
            }
        }
    }

    /// Serves `first`, then the mailbox, until the incarnation ends.
    async fn serve(mut self, first: Command, mailbox: &mut Mailbox<Command>) -> End {
        let mut command = first;
        loop {
            if let Err((cause, call)) = self.command(command, mailbox).await {
                return End::Failed(cause, call, self.waiting);
            }
            command = match mailbox.next().await {
                None => return End::Shutdown,
                Some(Next::Command(command)) => command,
                Some(Next::Stopped) => return End::Stopped(self.waiting),
                Some(Next::Ended(cause)) => return End::Failed(cause.into(), None, self.waiting),
            };
        }
    }

    /// Serves `command`; on a failure, its cause and the call to answer
    /// with it once the lease is released.
    async fn command(
        &mut self,
        command: Command,
        mailbox: &mut Mailbox<Command>,
    ) -> Result<(), (Error, Option<Stored>)> {
        match command {
            Command::Dispatch {
                dispatch_id,
                text,
                structured,
                respond_to,
            } => {
                if let Err(cause) = self.admit(dispatch_id, text, structured).await {
                    return Err((cause.into(), Some(respond_to)));
                }
                let _ = respond_to.send(Ok(()));
                self.start_next(mailbox);
            }
            Command::Read {
                dispatch_id,
                respond_to,
            } => match self.folded.settled.get(&dispatch_id) {
                Some(result) => {
                    let _ = respond_to.send(answer(dispatch_id, result));
                }
                None if self
                    .folded
                    .pending
                    .iter()
                    .any(|a| a.dispatch == dispatch_id) =>
                {
                    self.waiting
                        .entry(dispatch_id)
                        .or_default()
                        .push(respond_to);
                }
                None => {
                    let _ = respond_to.send(Err(Error::DispatchNotFound(dispatch_id)));
                }
            },
            Command::AbortQueued { respond_to } => {
                if let Err(cause) = self.abort(mailbox).await {
                    return Err((cause.into(), Some(respond_to)));
                }
                let _ = respond_to.send(Ok(()));
            }
            Command::Stepped {
                dispatch,
                run,
                step,
            } => {
                self.stepped(dispatch, *run, step, mailbox)
                    .await
                    .map_err(|cause| (cause.into(), None))?;
            }
        }
        Ok(())
    }

    /// Stores a dispatch, after the instance's `created` when the stream is empty.
    async fn admit(
        &mut self,
        dispatch_id: DispatchId,
        text: String,
        structured: Option<StructuredSpec>,
    ) -> Result<(), StoreError> {
        let mut events = Vec::new();
        if self.folded.last == Seq::default() {
            let data = self.start.data.clone();
            events.push((None, Event::Created { data }));
        }
        events.push((Some(dispatch_id), Event::Dispatched { text, structured }));
        self.append(events).await
    }

    /// Folds the stream from its first event, then settles a dispatch that
    /// was running when the last writer stopped.
    async fn recover(&mut self) -> Result<()> {
        let Start {
            name, id, store, ..
        } = self.start;
        for stored in store.read(name, id, Seq::default()).await? {
            let (seq, dispatch) = (stored.seq, stored.dispatch);
            let event = Event::of(stored).map_err(|reason| StoreError::Failed { reason })?;
            self.folded.apply(seq, dispatch, event);
        }
        if self
            .folded
            .data
            .as_ref()
            .is_some_and(|data| *data != self.start.data)
        {
            return Err(Error::InstanceDataConflict {
                agent: name.clone(),
                id: id.clone(),
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
    fn start_next(&mut self, mailbox: &mut Mailbox<Command>) {
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
        self.step(next.dispatch, run, mailbox);
    }

    /// Takes `run`'s next step for `dispatch`; it comes back as
    /// [`Command::Stepped`].
    fn step(&mut self, dispatch: DispatchId, run: Run, mailbox: &mut Mailbox<Command>) {
        self.stepping = true;
        mailbox.pipe_to_self(async move {
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
        mailbox: &mut Mailbox<Command>,
    ) -> Result<(), StoreError> {
        self.stepping = false;
        match step {
            Ok(Step::Append(events)) => {
                let events = events.into_iter().map(|e| (Some(dispatch), e)).collect();
                self.append(events).await?;
                mailbox.guard()?;
                self.step(dispatch, run, mailbox);
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
        self.start_next(mailbox);
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
    async fn abort(&mut self, mailbox: &mut Mailbox<Command>) -> Result<(), StoreError> {
        if self.folded.pending.is_empty() {
            return Ok(());
        }
        if self.stepping {
            mailbox.unpipe();
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
        } = self.start;
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
