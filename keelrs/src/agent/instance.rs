//! Agent instances: a cloneable [`AgentInstance`] handle and the actor behind it
//! (Alice Ryhl's "Actors with Tokio").
//!
//! The actor is the only writer of its instance's event stream, while it
//! holds the stream's lease. It stores each dispatch before returning its
//! receipt, runs dispatches one at a time, step by step, storing what each
//! step produced before the next, and answers reads from what the stream
//! adds up to ([`Folded`]).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::{BoxFuture, OptionFuture};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::agent::TurnSetup;
use crate::agent::rig::{self, Message, Run, Step};
use crate::builtin::Caches;
use crate::builtin::structured::{Structured, StructuredSpec};
use crate::event::Event;
use crate::store::{Epoch, EventId, Seq, Store, StoreError, StoredEvent};
use crate::{DispatchId, Error, InstanceId, Outcome, Result};

/// How many commands an instance's inbox holds before `dispatch` and `read` wait.
const INBOX_CAPACITY: usize = 1024;

/// What an aborted dispatch's unanswered tool calls are answered with.
const ABORTED: &str = "aborted";

/// One conversation with an agent, addressed by an id.
///
/// A handle to the instance's actor: clones reach the same instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    id: InstanceId,
    /// The actor's inbox.
    inbox: mpsc::Sender<Command>,
    /// The actor's control channel, served before the inbox: aborts.
    control: mpsc::Sender<oneshot::Sender<Result<()>>>,
}

impl AgentInstance {
    /// Spawns the actor for instance `id` of the agent named `name`, its turns
    /// set up by `setup`, created with `data` (as JSON) and kept in `store`;
    /// `shutdown` cancelling stops it.
    pub(crate) fn spawn(
        name: String,
        setup: TurnSetup,
        id: InstanceId,
        data: Value,
        store: Store,
        shutdown: &CancellationToken,
    ) -> Self {
        let (inbox, receiver) = mpsc::channel(INBOX_CAPACITY);
        let (control, controls) = mpsc::channel(1);
        let actor = InstanceActor {
            setup,
            name,
            id: id.clone(),
            data,
            inbox: receiver,
            control: controls,
            cancel: shutdown.child_token(),
            store,
            lease: None,
            running: None,
            waiting: HashMap::new(),
            caches: Caches::default(),
            folded: Folded::default(),
        };
        tokio::spawn(actor.run());
        Self { id, inbox, control }
    }

    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &InstanceId {
        &self.id
    }

    /// Submits `message`; returns a receipt, not the reply.
    ///
    /// The message is stored before the receipt returns, and runs after those
    /// dispatched before it. A store error, or another runtime holding the
    /// instance, shows up here; a model or connect error on [`read`](Self::read).
    pub async fn dispatch(&self, message: impl Into<String>) -> Result<Receipt> {
        self.submit(message.into(), None).await
    }

    /// [`dispatch`](Self::dispatch) then [`read`](Self::read): submits `message`
    /// and waits for its reply.
    pub async fn ask(&self, message: impl Into<String>) -> Result<Reply> {
        let receipt = self.dispatch(message).await?;
        self.read(&receipt).await
    }

    /// Submits `message` and waits for a result of type `T`: the model must end by calling
    /// `finish_structured` with `T`'s schema. Plain text is re-asked up to 3 times, then the
    /// dispatch settles as [`Outcome::Failed`].
    pub async fn ask_structured<T>(&self, message: impl Into<String>) -> Result<T>
    where
        T: DeserializeOwned + JsonSchema + 'static,
    {
        let Receipt { dispatch_id } = self
            .submit(message.into(), Some(StructuredSpec::of::<T>()))
            .await?;
        let failed = |reason: String| Error::Run {
            outcome: Outcome::Failed { reason },
            dispatch_id,
        };
        // The JSON the model sent, which the schema of `finish_structured` checked.
        let value = self
            .settled(dispatch_id)
            .await?
            .structured
            .ok_or_else(|| failed("settled without a structured result".to_owned()))?;
        serde_json::from_value(value).map_err(|e| failed(e.to_string()))
    }

    /// Waits for the receipt's message to settle and takes its reply.
    pub async fn read(&self, receipt: &Receipt) -> Result<Reply> {
        let dispatch_id = receipt.dispatch_id;
        let settled = self.settled(dispatch_id).await?;
        Ok(Reply {
            text: settled.text,
            dispatch_id,
        })
    }

    /// Stops the running dispatch and drops every queued one; each settles as
    /// [`Outcome::Aborted`]. With nothing running or queued, does nothing.
    ///
    /// Served before dispatches and reads already sent.
    pub async fn abort(&self) -> Result<()> {
        let (respond_to, response) = oneshot::channel();
        let _ = self.control.send(respond_to).await;
        // An actor that is gone, or stops before answering, has nothing left to abort.
        response.await.unwrap_or(Ok(()))
    }

    /// Submits `text`, asking for `structured` when given.
    async fn submit(&self, text: String, structured: Option<StructuredSpec>) -> Result<Receipt> {
        let dispatch_id = DispatchId::fresh();
        let (respond_to, response) = oneshot::channel();
        let command = Command::Dispatch {
            dispatch_id,
            text,
            structured,
            respond_to,
        };
        // A gone actor drops `respond_to` with the command: aborted, as below.
        let _ = self.inbox.send(command).await;
        response
            .await
            .unwrap_or_else(|_| Err(aborted(dispatch_id)))?;
        Ok(Receipt { dispatch_id })
    }

    /// Waits for `dispatch_id` to settle and takes what it settled with.
    async fn settled(&self, dispatch_id: DispatchId) -> Result<SettledValue> {
        let (respond_to, response) = oneshot::channel();
        let command = Command::Read {
            dispatch_id,
            respond_to,
        };
        let _ = self.inbox.send(command).await;
        // An actor that is gone, or stops before answering (it panicked), counts as aborted.
        response.await.unwrap_or_else(|_| Err(aborted(dispatch_id)))
    }
}

fn aborted(dispatch_id: DispatchId) -> Error {
    Error::Run {
        outcome: Outcome::Aborted,
        dispatch_id,
    }
}

/// What an [`AgentInstance`] sends its actor, in order.
#[derive(Debug)]
enum Command {
    Dispatch {
        dispatch_id: DispatchId,
        text: String,
        /// The structured result the message asks for, if any.
        structured: Option<StructuredSpec>,
        /// Answered once the dispatch is stored.
        respond_to: oneshot::Sender<Result<()>>,
    },
    Read {
        dispatch_id: DispatchId,
        respond_to: oneshot::Sender<Result<SettledValue>>,
    },
}

/// What a dispatched message settled with when it produced a reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SettledValue {
    /// The final assistant text.
    pub(crate) text: String,
    /// The structured result, when the message asked for one.
    pub(crate) structured: Option<Value>,
}

/// A dispatch stored and not yet settled.
#[derive(Debug, Clone)]
struct Admitted {
    dispatch: DispatchId,
    text: String,
    structured: Option<StructuredSpec>,
    /// Whether its run has stored anything yet.
    started: bool,
}

/// What the instance's event stream adds up to; changed only by `apply`.
#[derive(Debug, Default)]
struct Folded {
    /// The `seq` of the last event.
    last: Seq,
    /// The data the instance was created with.
    data: Option<Value>,
    /// Every message of every dispatch, as the next run's history.
    history: Vec<Message>,
    settled: HashMap<DispatchId, Result<SettledValue, Outcome>>,
    /// Stored dispatches not yet settled, in order; the first may be running.
    pending: VecDeque<Admitted>,
}

impl Folded {
    fn apply(&mut self, seq: Seq, dispatch: Option<DispatchId>, event: Event) {
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

/// The dispatch being run, and its step in flight.
struct Running {
    dispatch: DispatchId,
    step: BoxFuture<'static, (Run, Result<Step, String>)>,
}

/// Runs one agent instance's dispatches, one at a time, writing its events.
struct InstanceActor {
    setup: TurnSetup,
    /// The agent's name, which with `id` names the instance's event stream.
    name: String,
    id: InstanceId,
    /// The data the instance is created with, as JSON.
    data: Value,
    inbox: mpsc::Receiver<Command>,
    control: mpsc::Receiver<oneshot::Sender<Result<()>>>,
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
    /// Runs until the runtime is dropped or every handle is gone.
    async fn run(mut self) {
        loop {
            let renew_at = self.renew_at();
            select! {
                biased;
                () = self.cancel.cancelled() => break,
                Some(respond_to) = self.control.recv() => {
                    let _ = respond_to.send(self.abort().await);
                }
                Some((run, step)) = OptionFuture::from(self.running.as_mut().map(|r| &mut r.step)) => {
                    self.stepped(run, step).await;
                }
                () = tokio::time::sleep_until(renew_at), if self.lease.is_some() => self.renew().await,
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
        self.running = Some(Running {
            dispatch,
            step: Box::pin(run.step()),
        });
    }

    /// Stores what a step produced, then takes the next one.
    async fn stepped(&mut self, run: Run, step: Result<Step, String>) {
        let Some(Running { dispatch, .. }) = self.running.take() else {
            return;
        };
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
        let Some((epoch, _)) = self.lease else {
            return Err(self.reset(self.fenced()));
        };
        let at = now_ms();
        let expected = self.folded.last;
        let mut seq = expected;
        let stored: Vec<StoredEvent> = events
            .iter()
            .map(|(dispatch, event)| {
                seq = seq.next();
                let (kind, event) = event.to_parts();
                StoredEvent {
                    id: EventId::fresh(),
                    agent: self.name.clone(),
                    instance: self.id.clone(),
                    seq,
                    dispatch: *dispatch,
                    at,
                    kind,
                    event,
                }
            })
            .collect();
        if let Err(e) = self
            .store
            .append(&self.name, &self.id, epoch, expected, &stored)
            .await
        {
            return Err(self.reset(store_error(e)));
        }
        for (stored, (_, event)) in stored.iter().zip(events) {
            let settles = matches!(event, Event::Settled { .. });
            self.folded.apply(stored.seq, stored.dispatch, event);
            if settles
                && let Some(dispatch) = stored.dispatch
                && let Some(result) = self.folded.settled.get(&dispatch)
            {
                for read in self.waiting.remove(&dispatch).unwrap_or_default() {
                    let _ = read.send(answer(dispatch, result));
                }
            }
        }
        Ok(())
    }

    async fn renew(&mut self) {
        let Some((epoch, _)) = self.lease else {
            return;
        };
        let asked = Instant::now();
        match self.store.renew(&self.name, &self.id, epoch).await {
            Ok(()) => self.lease = Some((epoch, asked + trusted(&self.store))),
            Err(e) => {
                self.reset(store_error(e));
            }
        }
    }

    /// When to renew the lease: a third of its TTL after it was last taken or
    /// renewed (now, when not holding it; the branch is then disabled).
    fn renew_at(&self) -> Instant {
        let ttl = self.store.ttl();
        match self.lease {
            Some((_, deadline)) => {
                deadline - trusted(&self.store) + (ttl / 3).max(Duration::from_millis(1))
            }
            None => Instant::now(),
        }
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
fn closing(history: &[Message], dispatch: DispatchId) -> Vec<(Option<DispatchId>, Event)> {
    rig::close_pending(history, ABORTED)
        .into_iter()
        .map(|part| (Some(dispatch), Event::ToolResult(part)))
        .collect()
}

fn store_error(error: Error) -> StoreError {
    match error {
        Error::Store(error) => error,
        other => StoreError::Failed {
            reason: other.to_string(),
        },
    }
}

/// What a read of `dispatch_id` gets once it settled with `result`.
fn answer(dispatch_id: DispatchId, result: &Result<SettledValue, Outcome>) -> Result<SettledValue> {
    result.clone().map_err(|outcome| Error::Run {
        outcome,
        dispatch_id,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The claim ticket for one dispatched message: plain data, readable from any
/// handle to the instance that issued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    dispatch_id: DispatchId,
}

impl Receipt {
    /// The dispatched message this receipt claims.
    #[must_use]
    pub fn dispatch_id(&self) -> DispatchId {
        self.dispatch_id
    }
}

/// The settled outcome of one dispatched message.
#[derive(Debug, Clone)]
pub struct Reply {
    text: String,
    dispatch_id: DispatchId,
}

impl Reply {
    /// Final assistant text; empty when there is none.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The dispatched message this reply settles.
    #[must_use]
    pub fn dispatch_id(&self) -> DispatchId {
        self.dispatch_id
    }
}

#[cfg(test)]
mod tests;
