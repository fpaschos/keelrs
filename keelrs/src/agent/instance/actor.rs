//! The actor behind an [`AgentInstance`](super::AgentInstance): the only
//! writer of its instance's event stream while it holds the stream's lease.
//!
//! It lives one incarnation: it takes the lease and recovers the stream once
//! (its start), serves, and ends on any lease or store failure, failing its
//! callers with the cause. The next call re-attaches a new incarnation.

use std::collections::HashMap;

use futures::future::{BoxFuture, OptionFuture};
use serde_json::Value;
use tokio::select;
use tokio::sync::oneshot;

use super::fold::Folded;
use super::{Command, SettledValue};
use crate::actors::lease::Lease;
use crate::actors::{Mailbox, Next, sleep_until};
use crate::agent::TurnSetup;
use crate::agent::rig::{self, Connect, Message, Run, Step};
use crate::builtin::Caches;
use crate::builtin::structured::{Structured, StructuredSpec};
use crate::event::Event;
use crate::store::{self, Seq, Store, StoreError};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// What an aborted dispatch's unanswered tool calls are answered with.
const ABORTED: &str = "aborted";

/// Answered once what was asked is stored.
type Stored = oneshot::Sender<Result<()>>;

/// An abort, sent on the control channel; answered once it is stored.
pub(crate) type Control = Stored;

/// A parked read's answer.
type ReadReply = oneshot::Sender<Result<SettledValue>>;

/// The step in flight, with the dispatch it runs for.
type Running = BoxFuture<'static, Tick>;

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

/// The actor's own sources, beside its mailbox.
enum Tick {
    /// The running step finished.
    Stepped(DispatchId, Box<Run>, Result<Step, String>),
    /// The lease is due for renewal.
    Renew,
}

/// Runs one agent instance's dispatches, one at a time, writing its events.
struct InstanceActor {
    setup: TurnSetup,
    connect: Connect,
    /// The agent's name, which with `id` names the instance's event stream.
    name: String,
    id: InstanceId,
    data: Value,
    store: Store,
    lease: Lease,
    running: Option<Running>,
    /// Reads parked until their dispatch settles.
    waiting: HashMap<DispatchId, Vec<ReadReply>>,
    /// What this instance's built-in tools remember of its current sandbox; never stored.
    caches: Caches,
    folded: Folded,
}

/// The actor's life: start, serve, end. Commands sent during the start
/// wait in the inbox, in order.
pub(super) async fn run(start: Start, mut mailbox: Mailbox<Command, Control>) {
    let mut actor = match InstanceActor::start(start).await {
        Ok(actor) => actor,
        Err(cause) => {
            let (commands, controls) = mailbox.close();
            fail(commands, controls, Vec::new(), || again(&cause));
            return;
        }
    };
    let cause = actor.serve(&mut mailbox).await;
    actor.end(mailbox, cause).await;
}

impl InstanceActor {
    /// Takes the lease, recovers the stream and starts its first dispatch.
    async fn start(start: Start) -> Result<Self> {
        let lease =
            Lease::acquire(start.store.clone(), start.name.clone(), start.id.clone()).await?;
        let mut actor = Self {
            setup: start.setup,
            connect: start.connect,
            name: start.name,
            id: start.id,
            data: start.data,
            store: start.store,
            lease,
            running: None,
            waiting: HashMap::new(),
            caches: Caches::default(),
            folded: Folded::default(),
        };
        if let Err(e) = actor.recover().await {
            actor.lease.release().await;
            return Err(e);
        }
        actor.start_next();
        Ok(actor)
    }

    /// Serves until stopped (`None`) or a lease or store failure: its cause,
    /// and the failing call's reply, answered at the end.
    async fn serve(
        &mut self,
        mailbox: &mut Mailbox<Command, Control>,
    ) -> Option<(StoreError, Option<Stored>)> {
        loop {
            let running = OptionFuture::from(self.running.as_mut());
            let renew_at = self.lease.renew_at();
            let tick = async move {
                select! {
                    biased;
                    Some(tick) = running => tick,
                    () = sleep_until(Some(renew_at)) => Tick::Renew,
                }
            };
            let (served, reply) = match mailbox.next(tick).await? {
                Next::Control(respond_to) => (self.abort().await, Some(respond_to)),
                Next::Tick(Tick::Stepped(dispatch, run, step)) => {
                    (self.stepped(dispatch, *run, step).await, None)
                }
                Next::Tick(Tick::Renew) => (self.lease.renew().await, None),
                Next::Command(command) => self.command(command).await,
            };
            match served {
                Ok(()) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(Ok(()));
                    }
                }
                Err(cause) => return Some((cause, reply)),
            }
        }
    }

    /// Releases the lease and closes the mailbox, so a caller that retries
    /// re-attaches; then fails the failing call and every caller still
    /// waiting with the cause. On a stop, drops them (the runtime is gone).
    async fn end(
        self,
        mailbox: Mailbox<Command, Control>,
        failed: Option<(StoreError, Option<Stored>)>,
    ) {
        let Self { lease, waiting, .. } = self;
        lease.release().await;
        let (commands, controls) = mailbox.close();
        if let Some((cause, reply)) = failed {
            let error = || Error::from(cause.clone());
            if let Some(reply) = reply {
                let _ = reply.send(Err(error()));
            }
            let reads = waiting.into_values().flatten().collect();
            fail(commands, controls, reads, error);
        }
    }

    /// Serves `command`; a dispatch's reply waits until its admission is known.
    async fn command(&mut self, command: Command) -> (Result<(), StoreError>, Option<Stored>) {
        match command {
            Command::Dispatch {
                dispatch_id,
                text,
                structured,
                respond_to,
            } => {
                let admitted = self.admit(dispatch_id, text, structured).await;
                self.start_next();
                (admitted, Some(respond_to))
            }
            Command::Read {
                dispatch_id,
                respond_to,
            } => {
                match self.folded.settled.get(&dispatch_id) {
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
                }
                (Ok(()), None)
            }
        }
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
            let data = self.data.clone();
            events.push((None, Event::Created { data }));
        }
        events.push((Some(dispatch_id), Event::Dispatched { text, structured }));
        self.append(events).await
    }

    /// Folds the stream from its first event, then settles a dispatch that
    /// was running when the last writer stopped.
    async fn recover(&mut self) -> Result<()> {
        let stored = self
            .store
            .read(&self.name, &self.id, Seq::default())
            .await?;
        for stored in stored {
            let (seq, dispatch) = (stored.seq, stored.dispatch);
            let event = Event::of(stored).map_err(|reason| StoreError::Failed { reason })?;
            self.folded.apply(seq, dispatch, event);
        }
        if self
            .folded
            .data
            .as_ref()
            .is_some_and(|data| *data != self.data)
        {
            return Err(Error::InstanceDataConflict {
                agent: self.name.clone(),
                id: self.id.clone(),
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
    fn start_next(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(next) = self.folded.pending.front().cloned() else {
            return;
        };
        let run = Run::new(
            self.setup.clone(),
            next.text,
            self.folded.history.clone(),
            next.structured.map(Structured::from_spec),
            std::mem::take(&mut self.caches),
            self.connect.clone(),
        );
        self.run_step(next.dispatch, run);
    }

    /// Takes `run`'s next step for `dispatch`, kept until it finishes.
    fn run_step(&mut self, dispatch: DispatchId, run: Run) {
        self.running = Some(Box::pin(async move {
            let (run, step) = run.step().await;
            Tick::Stepped(dispatch, Box::new(run), step)
        }));
    }

    /// Stores what `dispatch`'s step produced, then takes the next one,
    /// unless the lease may have run out meanwhile.
    async fn stepped(
        &mut self,
        dispatch: DispatchId,
        run: Run,
        step: Result<Step, String>,
    ) -> Result<(), StoreError> {
        self.running = None;
        match step {
            Ok(Step::Append(events)) => {
                let events = events.into_iter().map(|e| (Some(dispatch), e)).collect();
                self.append(events).await?;
                if !self.lease.trusted() {
                    return Err(store::fenced(&self.name, &self.id));
                }
                self.run_step(dispatch, run);
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
        self.start_next();
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

    /// Stops the running dispatch and settles it and every queued one as aborted.
    async fn abort(&mut self) -> Result<(), StoreError> {
        if self.folded.pending.is_empty() {
            return Ok(());
        }
        if self.running.take().is_some() {
            // The run held them; what they remembered of the sandbox is lost.
            self.caches = Caches::default();
        }
        let mut events = closing(&self.folded.history, self.folded.pending[0].dispatch);
        for admitted in &self.folded.pending {
            let result = Err(Outcome::Aborted);
            events.push((Some(admitted.dispatch), Event::Settled { result }));
        }
        self.append(events).await?;
        self.start_next();
        Ok(())
    }

    /// Stores `events` after the stream's last, then folds them and answers
    /// the reads they settle.
    async fn append(&mut self, events: Vec<(Option<DispatchId>, Event)>) -> Result<(), StoreError> {
        let expected = self.folded.last;
        self.store
            .append(&self.name, &self.id, self.lease.epoch(), expected, &events)
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

/// Answers each parked read, queued command and queued control with `error`.
fn fail(
    commands: Vec<Command>,
    controls: Vec<Control>,
    reads: Vec<ReadReply>,
    error: impl Fn() -> Error,
) {
    commands
        .into_iter()
        .for_each(|command| command.fail(error()));
    for read in reads {
        let _ = read.send(Err(error()));
    }
    for control in controls {
        let _ = control.send(Err(error()));
    }
}

/// The error a start failed with, once more for each caller: a store
/// failure or an instance data conflict.
fn again(cause: &Error) -> Error {
    match cause {
        Error::InstanceDataConflict { agent, id } => Error::InstanceDataConflict {
            agent: agent.clone(),
            id: id.clone(),
        },
        Error::Store(e) => Error::Store(e.clone()),
        other => Error::Store(StoreError::Failed {
            reason: other.to_string(),
        }),
    }
}

/// An "aborted" result for each tool call `history` leaves unanswered.
pub(super) fn closing(
    history: &[Message],
    dispatch: DispatchId,
) -> Vec<(Option<DispatchId>, Event)> {
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
