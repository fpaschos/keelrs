//! The runtime, agent instances, and dispatching messages to them.
//!
//! Each agent instance is one task that owns its events and runs one dispatch at
//! a time; [`AgentInstance`] reaches it through a channel (Alice Ryhl's "Actors
//! with Tokio"). Nothing is persisted: events live in memory.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::driver::{self, Settled};
use crate::event::Event;
use crate::{Agent, Error, Outcome, Result};

/// The running keelrs instance that knows the agents and hosts their instances.
///
/// Dropping it stops every agent instance: runs in flight and queued settle as
/// [`Outcome::Aborted`].
#[derive(Debug)]
pub struct Runtime {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    agents: HashMap<String, Agent>,
    next_submission: AtomicU64,
    /// Running instance tasks, keyed by `(agent name, instance id)`.
    instances: Mutex<HashMap<(String, String), mpsc::Sender<Command>>>,
    /// Parent of every instance token; cancelled when the runtime is dropped.
    shutdown: CancellationToken,
}

impl Runtime {
    /// Starts a runtime hosting `agents`.
    pub fn start(agents: impl IntoIterator<Item = Agent>) -> Self {
        let agents = agents
            .into_iter()
            .map(|a| (a.name().to_owned(), a))
            .collect();
        Self {
            inner: Arc::new(Inner {
                agents,
                ..Inner::default()
            }),
        }
    }

    /// Addresses `agent` on this runtime; pick the instance with [`Init::id`].
    ///
    /// Creates nothing and never fails: an unknown agent errors on dispatch.
    #[must_use]
    pub fn init(&self, agent: &Agent) -> Init {
        Init {
            runtime: self.inner.clone(),
            agent: agent.name().to_owned(),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

/// An agent on a runtime, waiting for an instance id.
#[derive(Debug)]
pub struct Init {
    runtime: Arc<Inner>,
    agent: String,
}

impl Init {
    /// The instance with this id; the first dispatch creates it, later ones continue it.
    pub fn id(self, id: impl Into<String>) -> AgentInstance {
        AgentInstance {
            runtime: self.runtime,
            agent: self.agent,
            id: id.into(),
        }
    }
}

/// One conversation with an agent, addressed by an id.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    runtime: Arc<Inner>,
    agent: String,
    id: String,
}

impl AgentInstance {
    /// The id that addresses this instance.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Submits `message`; returns a receipt, not the reply.
    ///
    /// The message runs after those dispatched before it; a model or connect
    /// error shows up on [`read`](Self::read), not here.
    pub async fn dispatch(&self, message: impl Into<String>) -> Result<Receipt> {
        let agent = self
            .runtime
            .agents
            .get(&self.agent)
            .ok_or_else(|| Error::UnknownAgent(self.agent.clone()))?;
        let submission_id =
            SubmissionId(self.runtime.next_submission.fetch_add(1, Ordering::Relaxed));
        // The first dispatch to this id spawns its task.
        let inbox = self
            .runtime
            .instances
            .lock()
            .unwrap()
            .entry(self.key())
            .or_insert_with(|| InstanceTask::spawn(agent.clone(), &self.runtime.shutdown))
            .clone();
        let command = Command::Dispatch {
            submission_id,
            text: message.into(),
        };
        if inbox.send(command).await.is_err() {
            return Err(aborted(submission_id));
        }
        Ok(Receipt { submission_id })
    }

    /// Waits for the receipt's message to settle and takes its reply.
    pub async fn read(&self, receipt: &Receipt) -> Result<Reply> {
        if !self.runtime.agents.contains_key(&self.agent) {
            return Err(Error::UnknownAgent(self.agent.clone()));
        }
        let inbox = self
            .runtime
            .instances
            .lock()
            .unwrap()
            .get(&self.key())
            .cloned()
            .ok_or_else(|| Error::InstanceNotFound(self.id.clone()))?;
        let submission_id = receipt.submission_id;
        let (respond_to, response) = oneshot::channel();
        let command = Command::Read {
            submission_id,
            respond_to,
        };
        if inbox.send(command).await.is_err() {
            return Err(aborted(submission_id));
        }
        // A task that stops before answering was shut down.
        response
            .await
            .unwrap_or_else(|_| Err(aborted(submission_id)))
    }

    fn key(&self) -> (String, String) {
        (self.agent.clone(), self.id.clone())
    }
}

fn aborted(submission_id: SubmissionId) -> Error {
    Error::Run {
        outcome: Outcome::Aborted,
        submission_id,
    }
}

/// What an [`AgentInstance`] sends its task, in order.
#[derive(Debug)]
enum Command {
    Dispatch {
        submission_id: SubmissionId,
        text: String,
    },
    Read {
        submission_id: SubmissionId,
        respond_to: oneshot::Sender<Result<Reply>>,
    },
}

/// One dispatch in flight: the driver's future, which settles it.
struct Run {
    submission_id: SubmissionId,
    future: Pin<Box<dyn Future<Output = std::result::Result<Settled, String>> + Send>>,
}

/// A dispatched message that has not started.
struct Queued {
    submission_id: SubmissionId,
    text: String,
}

/// Owns one agent instance: its events, its queue, and the run in flight.
struct InstanceTask {
    agent: Agent,
    inbox: mpsc::Receiver<Command>,
    /// The instance token: cancelling it shuts the instance down.
    cancel: CancellationToken,
    /// Canonical events, in memory for now.
    events: Vec<Event>,
    queue: VecDeque<Queued>,
    waiters: HashMap<SubmissionId, Vec<oneshot::Sender<Result<Reply>>>>,
    current_run: Option<Run>,
}

impl InstanceTask {
    fn spawn(agent: Agent, shutdown: &CancellationToken) -> mpsc::Sender<Command> {
        let (inbox_tx, inbox) = mpsc::channel(64);
        let task = Self {
            agent,
            inbox,
            cancel: shutdown.child_token(),
            events: Vec::new(),
            queue: VecDeque::new(),
            waiters: HashMap::new(),
            current_run: None,
        };
        tokio::spawn(task.run());
        inbox_tx
    }

    async fn run(mut self) {
        loop {
            select! {
                biased;
                // 1. A kill always wins: the run in flight and everything queued abort.
                () = self.cancel.cancelled() => {
                    self.abort_all();
                    return;
                }
                // 2. Dispatch and Read, in order.
                command = self.inbox.recv() => match command {
                    Some(command) => self.handle(command),
                    // Every handle is gone; nobody can read what is left.
                    None => return,
                },
                // 3. The run in flight settles.
                (submission_id, result) = settle(&mut self.current_run) => {
                    self.current_run = None;
                    self.record(submission_id, result);
                    self.start_next();
                }
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Dispatch {
                submission_id,
                text,
            } => {
                self.queue.push_back(Queued {
                    submission_id,
                    text,
                });
                self.start_next();
            }
            Command::Read {
                submission_id,
                respond_to,
            } => {
                if let Some(result) = self.settled(submission_id) {
                    let _ = respond_to.send(result);
                } else if self.is_pending(submission_id) {
                    self.waiters
                        .entry(submission_id)
                        .or_default()
                        .push(respond_to);
                } else {
                    let _ = respond_to.send(Err(Error::SubmissionNotFound(submission_id)));
                }
            }
        }
    }

    /// Starts the next queued dispatch unless one is running: one run at a time.
    fn start_next(&mut self) {
        if self.current_run.is_some() {
            return;
        }
        let Some(Queued {
            submission_id,
            text,
        }) = self.queue.pop_front()
        else {
            return;
        };
        let history = self
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Message(message) => Some(message.clone()),
                Event::Settled { .. } => None,
            })
            .collect();
        self.current_run = Some(Run {
            submission_id,
            future: Box::pin(driver::run(self.agent.clone(), text, history)),
        });
    }

    /// Records how a run settled and answers its waiters.
    fn record(
        &mut self,
        submission_id: SubmissionId,
        result: std::result::Result<Settled, String>,
    ) {
        let result = match result {
            Ok(Settled { reply, messages }) => {
                self.events.extend(messages.into_iter().map(Event::Message));
                Ok(reply)
            }
            Err(reason) => Err(Outcome::Failed { reason }),
        };
        self.settle_with(submission_id, result);
    }

    fn settle_with(
        &mut self,
        submission_id: SubmissionId,
        result: std::result::Result<String, Outcome>,
    ) {
        for waiter in self.waiters.remove(&submission_id).unwrap_or_default() {
            let _ = waiter.send(answer(submission_id, &result));
        }
        self.events.push(Event::Settled {
            submission_id,
            result,
        });
    }

    /// Settles the run in flight and every queued dispatch as aborted.
    fn abort_all(&mut self) {
        // Dropping the run's future stops its model call.
        let current = self.current_run.take().map(|run| run.submission_id);
        let queued: Vec<_> = self.queue.drain(..).map(|q| q.submission_id).collect();
        for submission_id in current.into_iter().chain(queued) {
            self.settle_with(submission_id, Err(Outcome::Aborted));
        }
    }

    fn settled(&self, submission_id: SubmissionId) -> Option<Result<Reply>> {
        self.events.iter().find_map(|event| match event {
            Event::Settled {
                submission_id: id,
                result,
            } if *id == submission_id => Some(answer(submission_id, result)),
            _ => None,
        })
    }

    fn is_pending(&self, submission_id: SubmissionId) -> bool {
        self.current_run
            .as_ref()
            .is_some_and(|run| run.submission_id == submission_id)
            || self.queue.iter().any(|q| q.submission_id == submission_id)
    }
}

/// Waits for the run in flight; never resolves when there is none.
async fn settle(run: &mut Option<Run>) -> (SubmissionId, std::result::Result<Settled, String>) {
    match run {
        Some(run) => (run.submission_id, run.future.as_mut().await),
        None => std::future::pending().await,
    }
}

/// What a read of a settled submission gets.
fn answer(
    submission_id: SubmissionId,
    result: &std::result::Result<String, Outcome>,
) -> Result<Reply> {
    match result {
        Ok(text) => Ok(Reply {
            text: text.clone(),
            submission_id,
        }),
        Err(outcome) => Err(Error::Run {
            outcome: outcome.clone(),
            submission_id,
        }),
    }
}

/// Identifies one dispatched message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmissionId(u64);

impl fmt::Display for SubmissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The claim ticket for one dispatched message: plain data, readable from any
/// handle to the instance that issued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    submission_id: SubmissionId,
}

impl Receipt {
    /// The dispatched message this receipt claims.
    #[must_use]
    pub fn submission_id(&self) -> SubmissionId {
        self.submission_id
    }
}

/// The settled outcome of one dispatched message.
#[derive(Debug, Clone)]
pub struct Reply {
    text: String,
    submission_id: SubmissionId,
}

impl Reply {
    /// Final assistant text; empty when there is none.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The dispatched message this reply settles.
    #[must_use]
    pub fn submission_id(&self) -> SubmissionId {
        self.submission_id
    }
}
