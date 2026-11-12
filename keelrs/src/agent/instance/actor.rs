//! The actor behind an [`AgentInstance`](super::AgentInstance): the only
//! writer of its instance's event stream while it holds the stream's lease.

use std::collections::HashMap;
use std::time::Duration;

use futures::future::{BoxFuture, OptionFuture};
use serde_json::Value;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::fold::Folded;
use super::{Command, SettledValue};
use crate::agent::TurnSetup;
use crate::agent::rig::{self, Message, Run, Step};
use crate::builtin::Caches;
use crate::builtin::structured::{Structured, StructuredSpec};
use crate::event::Event;
use crate::store::{Epoch, Seq, Store, StoreError};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// How many commands an instance's inbox holds before `dispatch` and `read` wait.
const INBOX_CAPACITY: usize = 1024;

/// What an aborted dispatch's unanswered tool calls are answered with.
const ABORTED: &str = "aborted";

/// An abort, sent on the control channel; answered once it is stored.
pub(super) type Control = oneshot::Sender<Result<()>>;

/// The step in flight, with the dispatch it runs for.
type Running = BoxFuture<'static, (DispatchId, Run, Result<Step, String>)>;

/// Runs one agent instance's dispatches, one at a time, writing its events.
pub(super) struct InstanceActor {
    setup: TurnSetup,
    /// The agent's name, which with `id` names the instance's event stream.
    name: String,
    id: InstanceId,
    /// The data the instance is created with, as JSON.
    data: Value,
    inbox: mpsc::Receiver<Command>,
    control: mpsc::Receiver<Control>,
    /// Cancelled when the runtime is dropped.
    cancel: CancellationToken,
    store: Store,
    /// The lease's epoch, and when this actor stops trusting it (monotonic);
    /// `None` while not holding it.
    lease: Option<(Epoch, Instant)>,
    running: Option<Running>,
    /// Reads parked until their dispatch settles.
    waiting: HashMap<DispatchId, Vec<oneshot::Sender<Result<SettledValue>>>>,
    /// What this instance's built-in tools remember of its current sandbox; never stored.
    caches: Caches,
    folded: Folded,
}

impl InstanceActor {
    /// Spawns the actor for instance `id` of the agent named `name`, holding no
    /// lease and nothing folded until its first command; returns its inbox and
    /// its control channel.
    pub(super) fn spawn(
        name: String,
        setup: TurnSetup,
        id: InstanceId,
        data: Value,
        store: Store,
        cancel: CancellationToken,
    ) -> (mpsc::Sender<Command>, mpsc::Sender<Control>) {
        let (inbox, receiver) = mpsc::channel(INBOX_CAPACITY);
        let (control, controls) = mpsc::channel(1);
        let actor = Self {
            setup,
            name,
            id,
            data,
            inbox: receiver,
            control: controls,
            cancel,
            store,
            lease: None,
            running: None,
            waiting: HashMap::new(),
            caches: Caches::default(),
            folded: Folded::default(),
        };
        tokio::spawn(actor.run());
        (inbox, control)
    }

    /// Runs until the runtime is dropped or every handle is gone.
    async fn run(mut self) {
        loop {
            let renewal = OptionFuture::from(self.renewal());
            select! {
                biased;
                () = self.cancel.cancelled() => break,
                Some(respond_to) = self.control.recv() => {
                    let _ = respond_to.send(self.abort().await);
                }
                Some((dispatch, run, step)) = OptionFuture::from(self.running.as_mut()) => {
                    self.stepped(dispatch, run, step).await;
                }
                Some(epoch) = renewal => self.renew(epoch).await,
                command = self.inbox.recv() => match command {
                    Some(command) => self.command(command).await,
                    None => break,
                },
            }
        }
        self.release().await;
    }

    /// Gives up the lease, if held, best-effort.
    async fn release(&mut self) {
        if let Some((epoch, _)) = self.lease.take() {
            let _ = self.store.release(&self.name, &self.id, epoch).await;
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Dispatch {
                dispatch_id,
                text,
                structured,
                respond_to,
            } => {
                let admitted = self.admit(dispatch_id, text, structured).await;
                let _ = respond_to.send(admitted);
                self.start_next();
            }
            Command::Read {
                dispatch_id,
                respond_to,
            } => {
                if let Err(e) = self.hold().await {
                    let _ = respond_to.send(Err(e));
                    return;
                }
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
            }
        }
    }

    /// Stores a dispatch, after the instance's `created` when the stream is empty.
    async fn admit(
        &mut self,
        dispatch_id: DispatchId,
        text: String,
        structured: Option<StructuredSpec>,
    ) -> Result<()> {
        self.hold().await?;
        let mut events = Vec::new();
        if self.folded.last == Seq::default() {
            let data = self.data.clone();
            events.push((None, Event::Created { data }));
        }
        events.push((Some(dispatch_id), Event::Dispatched { text, structured }));
        self.append(events).await
    }

    /// Takes the lease and recovers the stream, unless already holding it,
    /// then starts the first recovered dispatch.
    async fn hold(&mut self) -> Result<()> {
        if self.lease.is_some() {
            return Ok(());
        }
        let asked = Instant::now();
        let epoch = self.store.acquire(&self.name, &self.id).await?;
        self.lease = Some((epoch, asked + trusted(&self.store)));
        if let Err(e) = self.recover().await {
            self.release().await;
            return Err(e);
        }
        self.start_next();
        Ok(())
    }

    /// Folds the stream from its first event, then settles a dispatch that
    /// was running when the last writer stopped.
    async fn recover(&mut self) -> Result<()> {
        let stored = self
            .store
            .read(&self.name, &self.id, Seq::default())
            .await?;
        let mut folded = Folded::default();
        for stored in stored {
            let (seq, dispatch) = (stored.seq, stored.dispatch);
            let event = Event::of(stored).map_err(|reason| StoreError::Failed { reason })?;
            folded.apply(seq, dispatch, event);
        }
        if folded.data.as_ref().is_some_and(|data| *data != self.data) {
            return Err(Error::InstanceDataConflict {
                agent: self.name.clone(),
                id: self.id.clone(),
            });
        }
        self.folded = folded;
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
        );
        self.run_step(next.dispatch, run);
    }

    /// Takes `run`'s next step for `dispatch`, kept until it finishes.
    fn run_step(&mut self, dispatch: DispatchId, run: Run) {
        self.running = Some(Box::pin(async move {
            let (run, step) = run.step().await;
            (dispatch, run, step)
        }));
    }

    /// Stores what `dispatch`'s step produced, then takes the next one.
    async fn stepped(&mut self, dispatch: DispatchId, run: Run, step: Result<Step, String>) {
        self.running = None;
        let stored = match step {
            Ok(Step::Append(events)) => {
                let events = events.into_iter().map(|e| (Some(dispatch), e)).collect();
                match self.append(events).await {
                    Ok(()) if self.trusts_lease() => {
                        self.run_step(dispatch, run);
                        return;
                    }
                    Ok(()) => Err(self.reset(self.fenced())),
                    Err(e) => Err(e),
                }
            }
            Ok(Step::Settled(events, value)) => {
                self.caches = run.into_caches();
                self.settle(dispatch, events, Ok(value)).await
            }
            Err(reason) => {
                self.caches = run.into_caches();
                self.settle(dispatch, Vec::new(), Err(Outcome::Failed { reason }))
                    .await
            }
        };
        if stored.is_ok() {
            self.start_next();
        }
    }

    /// Stores `events` and how `dispatch` settled in one append, answering
    /// unanswered tool calls first when it did not finish.
    async fn settle(
        &mut self,
        dispatch: DispatchId,
        events: Vec<Event>,
        result: Result<SettledValue, Outcome>,
    ) -> Result<()> {
        let mut events: Vec<_> = events.into_iter().map(|e| (Some(dispatch), e)).collect();
        if result.is_err() {
            events.extend(closing(&self.folded.history, dispatch));
        }
        events.push((Some(dispatch), Event::Settled { result }));
        self.append(events).await
    }

    /// Stops the running dispatch and settles it and every queued one as aborted.
    async fn abort(&mut self) -> Result<()> {
        // What is queued is known only once the stream is recovered.
        self.hold().await?;
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
    /// the reads they settle. Any store failure resets the actor.
    async fn append(&mut self, events: Vec<(Option<DispatchId>, Event)>) -> Result<()> {
        let (epoch, _) = self.lease.expect("every append follows `hold`");
        let expected = self.folded.last;
        if let Err(e) = self
            .store
            .append(&self.name, &self.id, epoch, expected, &events)
            .await
        {
            return Err(self.reset(e));
        }
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

    async fn renew(&mut self, epoch: Epoch) {
        let asked = Instant::now();
        match self.store.renew(&self.name, &self.id, epoch).await {
            Ok(()) => self.lease = Some((epoch, asked + trusted(&self.store))),
            Err(e) => {
                self.reset(e);
            }
        }
    }

    /// While holding the lease: a wait until it is due for renewal, a third
    /// of its TTL after it was last taken or renewed, yielding its epoch.
    fn renewal(&self) -> Option<impl Future<Output = Epoch> + use<>> {
        let (epoch, deadline) = self.lease?;
        let ttl = self.store.ttl();
        let at = deadline - trusted(&self.store) + (ttl / 3).max(Duration::from_millis(1));
        Some(async move {
            tokio::time::sleep_until(at).await;
            epoch
        })
    }

    /// Whether the lease can still be trusted to start a step.
    fn trusts_lease(&self) -> bool {
        self.lease
            .is_some_and(|(_, deadline)| Instant::now() < deadline)
    }

    fn fenced(&self) -> StoreError {
        StoreError::Fenced {
            agent: self.name.clone(),
            id: self.id.clone(),
        }
    }

    /// Drops the run, the fold and the lease after a store failure, failing
    /// every parked read with it; the next command recovers afresh.
    fn reset(&mut self, error: StoreError) -> Error {
        self.running = None;
        self.lease = None;
        self.folded = Folded::default();
        self.caches = Caches::default();
        for (_, reads) in self.waiting.drain() {
            for read in reads {
                let _ = read.send(Err(error.clone().into()));
            }
        }
        error.into()
    }
}

/// How long after asking for a lease the actor trusts it: its TTL less a tenth.
fn trusted(store: &Store) -> Duration {
    store.ttl() - store.ttl() / 10
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
