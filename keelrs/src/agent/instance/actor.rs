//! The actor behind an [`AgentInstance`](super::AgentInstance): the only
//! writer of its instance's event stream while it holds the stream's lease.
//!
//! Each start takes the lease and recovers the stream; the actor then runs
//! dispatches one at a time, each as one piped [`Run`], renews its lease on a
//! scheduled [`Command::Renew`], and stops on any lease or store failure,
//! releasing the lease. The library starts it again on the next command.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::oneshot;

use super::fold::Folded;
use super::lease::Lease;
use super::{Command, SettledValue};
use crate::actors::{Actor, ActorRef, Context, PipeGuard};
use crate::agent::TurnSetup;
use crate::agent::rig::{self, Connect, Message, PersistSink, Run};
use crate::builtin::Caches;
use crate::builtin::structured::{Structured, StructuredSpec};
use crate::event::Event;
use crate::store::{Seq, Store, StoreError};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// What an aborted dispatch's unanswered tool calls are answered with.
const ABORTED: &str = "aborted";

/// What the actor is built from: instance `id` of the agent named `name`.
pub(super) struct InstanceArgs {
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
    args: Arc<InstanceArgs>,
    /// Held for the actor's life; its epoch goes with every append.
    lease: Lease,
    /// The running dispatch's run; dropping it cancels the run.
    running: Option<PipeGuard>,
    /// Reads parked until their dispatch settles.
    waiting: HashMap<DispatchId, Vec<oneshot::Sender<Result<SettledValue>>>>,
    /// What this instance's built-in tools remember of its current sandbox; never stored.
    caches: Caches,
    folded: Folded,
}

/// The run's sink: asks the actor to store, one persist at a time.
struct Writer {
    me: ActorRef<Command>,
    dispatch: DispatchId,
}

impl PersistSink for Writer {
    async fn persist(&mut self, events: Vec<Event>) {
        let stored = self
            .me
            .ask(|respond_to| Command::Persist {
                dispatch: self.dispatch,
                events,
                respond_to,
            })
            .await;
        if stored.is_none() {
            // The actor stopped; it drops this run.
            std::future::pending::<()>().await;
        }
    }
}

impl Actor for InstanceActor {
    type Command = Command;
    type Args = Arc<InstanceArgs>;

    /// Takes the lease, recovers the stream and starts its first dispatch.
    async fn start(args: Arc<InstanceArgs>, cx: &mut Context<Command>) -> Result<Self> {
        let lease = Lease::acquire(args.store.clone(), args.name.clone(), args.id.clone()).await?;
        let mut actor = Self {
            args,
            lease,
            running: None,
            waiting: HashMap::new(),
            caches: Caches::default(),
            folded: Folded::default(),
        };
        if let Err(cause) = actor.recover().await {
            actor.lease.release().await;
            return Err(cause);
        }
        cx.schedule(actor.lease.renew_at(), Command::Renew);
        actor.start_next(cx);
        Ok(actor)
    }

    async fn handle(&mut self, command: Command, cx: &mut Context<Command>) -> Result<()> {
        match command {
            Command::Dispatch {
                data, respond_to, ..
            } if !Arc::ptr_eq(&data, &self.args.data) && *data != *self.args.data => {
                let _ = respond_to.send(Err(self.data_conflict()));
            }
            Command::Dispatch {
                dispatch_id,
                text,
                structured,
                respond_to,
                ..
            } => {
                // An error drops `respond_to`: its caller gets `InstanceUnavailable`.
                self.admit(dispatch_id, &text, structured.as_ref()).await?;
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
            Command::Abort { respond_to } => {
                self.abort().await?;
                let _ = respond_to.send(Ok(()));
            }
            Command::Persist {
                dispatch,
                events,
                respond_to,
            } => {
                // Stale: an abort settled its dispatch and dropped its run.
                if self
                    .folded
                    .pending
                    .front()
                    .is_none_or(|a| a.dispatch != dispatch)
                {
                    return Ok(());
                }
                self.append(tagged(dispatch, events)).await?;
                // Tools start only after a guarded write.
                self.lease.guard()?;
                let _ = respond_to.send(());
            }
            Command::Finished {
                dispatch,
                completed,
            } => {
                self.running = None;
                self.caches = completed.caches;
                let result = completed
                    .result
                    .map_err(|reason| Outcome::Failed { reason });
                self.settle(dispatch, completed.events, result).await?;
                self.start_next(cx);
            }
            Command::Renew => {
                self.lease.renew().await?;
                cx.schedule(self.lease.renew_at(), Command::Renew);
            }
        }
        Ok(())
    }

    /// Releases the lease, then answers the parked reads with the cause of a
    /// failure.
    async fn post_stop(self, cause: Option<&Error>, _: &mut Context<Command>) {
        self.lease.release().await;
        if let Some(cause) = cause {
            for respond_to in self.waiting.into_values().flatten() {
                let _ = respond_to.send(Err(cause.clone()));
            }
        }
    }

    /// Answers `first` with the cause: `Held` tells the caller that another
    /// runtime holds the instance.
    fn start_failed(first: Command, cause: &Error) {
        match first {
            Command::Dispatch { respond_to, .. } | Command::Abort { respond_to } => {
                let _ = respond_to.send(Err(cause.clone()));
            }
            Command::Read { respond_to, .. } => {
                let _ = respond_to.send(Err(cause.clone()));
            }
            // Never first: the actor sends them to itself while it runs.
            Command::Persist { .. } | Command::Finished { .. } | Command::Renew => {}
        }
    }
}

impl InstanceActor {
    /// The instance exists with other data than asked for.
    fn data_conflict(&self) -> Error {
        Error::InstanceDataConflict {
            agent: self.args.name.clone(),
            id: self.args.id.clone(),
        }
    }

    /// Stores a dispatch, after the instance's `created` when the stream is empty.
    async fn admit(
        &mut self,
        dispatch_id: DispatchId,
        text: &str,
        structured: Option<&StructuredSpec>,
    ) -> Result<(), StoreError> {
        let mut events = Vec::new();
        if self.folded.last == Seq::default() {
            let data = Value::clone(&self.args.data);
            events.push((None, Event::Created { data }));
        }
        let (text, structured) = (text.to_owned(), structured.cloned());
        events.push((Some(dispatch_id), Event::Dispatched { text, structured }));
        self.append(events).await
    }

    /// Folds the stream from its first event, then settles a dispatch that
    /// was running when the last writer stopped.
    async fn recover(&mut self) -> Result<()> {
        let InstanceArgs {
            name, id, store, ..
        } = &*self.args;
        for stored in store.read(name, id, Seq::default()).await? {
            let (seq, dispatch) = (stored.seq, stored.dispatch);
            let event = Event::of(stored).map_err(|reason| StoreError::Failed { reason })?;
            self.folded.apply(seq, dispatch, event);
        }
        if self
            .folded
            .data
            .as_ref()
            .is_some_and(|data| *data != *self.args.data)
        {
            return Err(self.data_conflict());
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

    /// Starts the first stored dispatch as a piped run, unless one is running;
    /// it comes back as [`Command::Finished`].
    fn start_next(&mut self, cx: &mut Context<Command>) {
        if self.running.is_some() {
            return;
        }
        let Some(next) = self.folded.pending.front().cloned() else {
            return;
        };
        let dispatch = next.dispatch;
        let run = Run::new(
            self.args.setup.clone(),
            next.text,
            self.folded.history.clone(),
            next.structured.map(Structured::from_spec),
            std::mem::take(&mut self.caches),
            self.args.connect.clone(),
        );
        let me = cx.self_ref();
        self.running = Some(cx.pipe_to_self(async move {
            let completed = run.drive(Writer { me, dispatch }).await;
            Command::Finished {
                dispatch,
                completed,
            }
        }));
    }

    /// Stores `events` and how `dispatch` settled in one append, answering
    /// unanswered tool calls first when it did not finish.
    async fn settle(
        &mut self,
        dispatch: DispatchId,
        events: Vec<Event>,
        result: Result<SettledValue, Outcome>,
    ) -> Result<(), StoreError> {
        let mut events = tagged(dispatch, events);
        if result.is_err() {
            events.extend(closing(&self.folded.history, dispatch));
        }
        events.push((Some(dispatch), Event::Settled { result }));
        self.append(events).await
    }

    /// Cancels the running dispatch and settles every queued one as aborted.
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
        self.append(events).await
    }

    /// Stores `events` after the stream's last, then folds them and answers
    /// the reads they settle.
    async fn append(&mut self, events: Vec<(Option<DispatchId>, Event)>) -> Result<(), StoreError> {
        let InstanceArgs {
            name, id, store, ..
        } = &*self.args;
        let expected = self.folded.last;
        store
            .append(name, id, self.lease.epoch(), expected, &events)
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

/// `events`, each tagged with `dispatch`.
fn tagged(dispatch: DispatchId, events: Vec<Event>) -> Vec<(Option<DispatchId>, Event)> {
    events.into_iter().map(|e| (Some(dispatch), e)).collect()
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
