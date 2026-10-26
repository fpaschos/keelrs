//! The agent actor: one task per agent instance (Alice Ryhl's "Actors with Tokio").
//!
//! It owns the instance's events and takes commands from its inbox one at a time,
//! running each dispatch to settled before the next. Nothing is persisted: events
//! live in memory.

use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::rig::{self, Settled};
use crate::event::Event;
use crate::{Agent, Error, Outcome, Reply, Result, SubmissionId};

/// How many commands an instance's inbox holds before `dispatch` and `read` wait.
const INBOX_CAPACITY: usize = 1024;

/// What an [`AgentInstance`](crate::AgentInstance) sends its task, in order.
#[derive(Debug)]
pub(crate) enum Command {
    Dispatch {
        submission_id: SubmissionId,
        text: String,
    },
    Read {
        submission_id: SubmissionId,
        respond_to: oneshot::Sender<Result<Reply>>,
    },
}

/// Owns one agent instance: its events, and the one dispatch it runs at a time.
///
/// The inbox is the queue: commands run in the order they were sent, and a
/// dispatch runs to settled before the next command, so a read always finds its
/// message settled.
pub(crate) struct InstanceTask {
    agent: Agent,
    inbox: mpsc::Receiver<Command>,
    /// The instance token: cancelling it aborts the run in flight and every later dispatch.
    cancel: CancellationToken,
    /// Canonical events, in memory for now.
    events: Vec<Event>,
}

impl InstanceTask {
    pub(crate) fn spawn(agent: Agent, shutdown: &CancellationToken) -> mpsc::Sender<Command> {
        let (inbox_tx, inbox) = mpsc::channel(INBOX_CAPACITY);
        let task = Self {
            agent,
            inbox,
            cancel: shutdown.child_token(),
            events: Vec::new(),
        };
        tokio::spawn(task.run());
        inbox_tx
    }

    /// Runs until every handle is gone.
    async fn run(mut self) {
        while let Some(command) = self.inbox.recv().await {
            match command {
                Command::Dispatch {
                    submission_id,
                    text,
                } => self.dispatch(submission_id, text).await,
                Command::Read {
                    submission_id,
                    respond_to,
                } => {
                    let result = self
                        .settled(submission_id)
                        .unwrap_or(Err(Error::SubmissionNotFound(submission_id)));
                    let _ = respond_to.send(result);
                }
            }
        }
    }

    /// Runs one dispatch to settled and records it; a kill aborts it.
    async fn dispatch(&mut self, submission_id: SubmissionId, text: String) {
        let history = self
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Message(message) => Some(message.clone()),
                Event::Settled { .. } => None,
            })
            .collect();
        let result = select! {
            biased;
            // Dropping the run's future stops its model call.
            () = self.cancel.cancelled() => Err(Outcome::Aborted),
            result = rig::run(self.agent.clone(), text, history) => match result {
                Ok(Settled { reply, messages }) => {
                    self.events.extend(messages.into_iter().map(Event::Message));
                    Ok(reply)
                }
                Err(reason) => Err(Outcome::Failed { reason }),
            },
        };
        self.events.push(Event::Settled {
            submission_id,
            result,
        });
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
