//! The keelrs actor pattern: Alice Ryhl's "Actors with Tokio", with a few
//! Akka ideas. Not a framework: each actor writes its own loop.
//!
//! Three layers, each opt-in over the one before:
//!
//! - **base** (this file): a [`Handle`] to send to, and a [`Mailbox`] the
//!   actor's task reads: a priority control channel, its own sources (a
//!   running step, a deadline) and the inbox. An actor runs its start, then
//!   its loop, then its end, in plain code: while the start runs, commands
//!   wait in the inbox in order. It lives one incarnation.
//! - **keyed** ([`entity`]): an [`EntityRef`](entity::EntityRef) names an
//!   actor by key, not by its task, and re-attaches to a new incarnation.
//! - **persistent** ([`lease`]): a [`Lease`](lease::Lease) makes the actor
//!   the single writer of its stream.

pub(crate) mod entity;
pub(crate) mod lease;

use std::future::{Future, pending};

use tokio::select;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How many commands an inbox holds before senders wait.
const INBOX_CAPACITY: usize = 1024;

/// How many control messages wait before senders wait.
const CONTROL_CAPACITY: usize = 1;

/// Where an actor's commands (`C`) and control messages (`K`) are sent.
pub(crate) struct Handle<C, K> {
    inbox: mpsc::Sender<C>,
    control: mpsc::Sender<K>,
}

impl<C, K> Clone for Handle<C, K> {
    fn clone(&self) -> Self {
        Self {
            inbox: self.inbox.clone(),
            control: self.control.clone(),
        }
    }
}

impl<C, K> Handle<C, K> {
    fn inbox(&self) -> &mpsc::Sender<C> {
        &self.inbox
    }

    fn control(&self) -> &mpsc::Sender<K> {
        &self.control
    }

    /// Whether the actor ended: its inbox no longer takes commands.
    fn is_closed(&self) -> bool {
        self.inbox.is_closed()
    }
}

/// Why an ask got no answer.
#[derive(Debug)]
pub(crate) enum AskError<C> {
    /// The inbox was closed: the actor never saw the message; a resend is safe.
    NotDelivered(C),
    /// The actor took the message, then ended without answering; a resend is not safe.
    NoReply,
}

/// What an actor's task reads: its control channel, its inbox, and the
/// token that stops it.
pub(crate) struct Mailbox<C, K> {
    inbox: mpsc::Receiver<C>,
    control: mpsc::Receiver<K>,
    cancel: CancellationToken,
}

/// What [`Mailbox::next`] yields, in priority order.
pub(crate) enum Next<C, K, T> {
    Control(K),
    /// One of the actor's own sources finished.
    Tick(T),
    Command(C),
}

impl<C, K> Mailbox<C, K> {
    /// The next thing to serve: cancel, then control, then `tick`, then the
    /// inbox. `None` once stopped, or once every handle is gone.
    pub(crate) async fn next<T>(&mut self, tick: impl Future<Output = T>) -> Option<Next<C, K, T>> {
        select! {
            biased;
            () = self.cancel.cancelled() => None,
            Some(control) = self.control.recv() => Some(Next::Control(control)),
            tick = tick => Some(Next::Tick(tick)),
            command = self.inbox.recv() => command.map(Next::Command),
        }
    }

    /// Closes the inbox and the control channel, so later sends fail as
    /// [`AskError::NotDelivered`]; returns what was queued, to answer.
    pub(crate) fn close(mut self) -> (Vec<C>, Vec<K>) {
        self.inbox.close();
        self.control.close();
        let mut commands = Vec::new();
        while let Ok(command) = self.inbox.try_recv() {
            commands.push(command);
        }
        let mut controls = Vec::new();
        while let Ok(control) = self.control.try_recv() {
            controls.push(control);
        }
        (commands, controls)
    }
}

/// Spawns an actor's task `run` on `rt`, stopped when `cancel` is; returns
/// its handle.
pub(crate) fn spawn<C, K, F>(
    rt: &tokio::runtime::Handle,
    cancel: CancellationToken,
    run: impl FnOnce(Mailbox<C, K>) -> F,
) -> Handle<C, K>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (inbox, inbox_rx) = mpsc::channel(INBOX_CAPACITY);
    let (control, control_rx) = mpsc::channel(CONTROL_CAPACITY);
    rt.spawn(run(Mailbox {
        inbox: inbox_rx,
        control: control_rx,
        cancel,
    }));
    Handle { inbox, control }
}

/// Waits until `at`; forever when `None`. An actor's timers are deadlines
/// kept in its state.
pub(crate) async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
    }
}
