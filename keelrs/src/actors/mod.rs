//! The keelrs actor pattern: Alice Ryhl's "Actors with Tokio", with a few
//! Akka ideas. The library owns the loop; an actor gives three hooks
//! ([`Actor::start`], [`Actor::handle`], [`Actor::stopped`]).
//!
//! Three layers, each opt-in over the one before:
//!
//! - **core** (this file): an [`ActorRef`] to send to, and one task per
//!   actor that runs it. The actor restarts in place inside that task: each
//!   start builds a new actor from its [`Actor::Args`], so its `ActorRef`
//!   never changes.
//! - **keyed** ([`entity`]): [`Entities`](entity::Entities) holds every actor
//!   of one type by key.
//! - **persistent** ([`lease`]): a [`Lease`] the [`Context`] holds makes the
//!   actor the single writer of its stream.

pub(crate) mod entity;
pub(crate) mod lease;

use std::collections::VecDeque;
use std::fmt;
use std::future::{Future, pending};

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use self::lease::Lease;
use crate::store::StoreError;
use crate::{Error, Result};

/// How many commands an inbox holds before senders wait.
const INBOX_CAPACITY: usize = 1024;

/// How many system messages wait; a `Stop` already waiting is enough.
const SYSTEM_CAPACITY: usize = 1;

/// An actor: its commands, the arguments each start builds it from, and its
/// hooks. The library calls them from one task, one at a time.
pub(crate) trait Actor: Sized + Send + 'static {
    type Command: Command;
    /// What each start builds the actor from; kept for the actor's life.
    type Args: Send + Sync + 'static;

    /// Builds the actor, on the first command after each stop. Commands sent
    /// meanwhile wait in the inbox, in order.
    fn start(
        args: &Self::Args,
        cx: &mut Context<Self::Command>,
    ) -> impl Future<Output = Result<Self>> + Send;

    /// Serves one command. An error stops the actor with that cause; a
    /// command left unanswered by it is stashed, and fails with the queue.
    fn handle(
        &mut self,
        command: Self::Command,
        cx: &mut Context<Self::Command>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// The actor is stopped: what it piped is dropped and its lease released.
    /// It stashes what it still owes an answer and answers nothing itself.
    /// On [`Stop::Failed`] the library then fails the stash and the inbox
    /// with the cause; on [`Stop::Graceful`] they wait for the next start.
    fn stopped(self, why: Stop, cx: &mut Context<Self::Command>)
    -> impl Future<Output = ()> + Send;
}

/// A command an actor's inbox carries.
pub(crate) trait Command: Send + 'static {
    /// Answers the command with `error`, unserved.
    fn fail(self, error: Error);
}

/// Why an actor stopped.
pub(crate) enum Stop {
    /// [`ActorRef::stop`].
    Graceful,
    /// A start, a command or the lease's renewal failed.
    Failed(Error),
}

/// Where an actor's commands (`C`) are sent: the same actor for good.
pub(crate) struct ActorRef<C> {
    inbox: mpsc::Sender<C>,
    system: mpsc::Sender<System>,
}

impl<C> Clone for ActorRef<C> {
    fn clone(&self) -> Self {
        Self {
            inbox: self.inbox.clone(),
            system: self.system.clone(),
        }
    }
}

impl<C> fmt::Debug for ActorRef<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorRef").finish_non_exhaustive()
    }
}

impl<C> ActorRef<C> {
    /// Sends the command `make` builds and waits for its answer. `None`: no
    /// actor answered (the runtime was dropped).
    pub(crate) async fn ask<R>(&self, make: impl FnOnce(oneshot::Sender<R>) -> C) -> Option<R> {
        let (respond_to, answer) = oneshot::channel();
        self.inbox.send(make(respond_to)).await.ok()?;
        answer.await.ok()
    }

    /// Stops the actor gracefully, before the commands already queued.
    pub(crate) fn stop(&self) {
        // Full: a `Stop` is already waiting.
        let _ = self.system.try_send(System::Stop);
    }
}

/// The library's own messages, served before the inbox.
enum System {
    Stop,
}

/// What an actor's hooks are given: its mailbox, as far as an actor may
/// touch it.
pub(crate) struct Context<C> {
    inbox: mpsc::Receiver<C>,
    system: mpsc::Receiver<System>,
    stashed: VecDeque<C>,
    piped: FuturesUnordered<BoxFuture<'static, C>>,
    cancel: CancellationToken,
    lease: Option<Lease>,
}

/// What [`Context::next`] yields.
enum Next<C> {
    Command(C),
    Stopped,
    LeaseLost(StoreError),
}

impl<C> Context<C> {
    /// Runs `future` in the actor's task, as Akka's `pipeToSelf`: its output
    /// comes back as a command once the inbox is empty.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "a write to the mailbox; `&mut` keeps `Context` free of a `Sync` bound"
    )]
    pub(crate) fn pipe_to_self(&mut self, future: impl Future<Output = C> + Send + 'static) {
        self.piped.push(Box::pin(future));
    }

    /// Drops every future piped so far: they stop at once.
    pub(crate) fn unpipe(&mut self) {
        self.piped.clear();
    }

    /// Keeps `command` for the next start, served before the inbox.
    pub(crate) fn stash(&mut self, command: C) {
        self.stashed.push_back(command);
    }

    /// Holds `lease`: renewed by the library, released when the actor stops.
    pub(crate) fn hold(&mut self, lease: Lease) {
        self.lease = Some(lease);
    }

    /// The deadline guard: [`StoreError::Fenced`] unless the held lease is
    /// still trusted.
    pub(crate) fn guard(&self) -> Result<(), StoreError> {
        self.lease.as_ref().map_or(Ok(()), Lease::guard)
    }

    /// The next thing to serve, in order: cancel (`None`), the stash, the
    /// system lane, the held lease's renewal, the inbox, then what was piped.
    async fn next(&mut self) -> Option<Next<C>> {
        if self.cancel.is_cancelled() {
            return None;
        }
        if let Some(command) = self.stashed.pop_front() {
            return Some(Next::Command(command));
        }
        loop {
            let renew_at = self.lease.as_ref().map(Lease::renew_at);
            select! {
                biased;
                () = self.cancel.cancelled() => return None,
                Some(System::Stop) = self.system.recv() => return Some(Next::Stopped),
                () = sleep_until(renew_at) => {
                    if let Some(lease) = &mut self.lease
                        && let Err(cause) = lease.renew().await
                    {
                        return Some(Next::LeaseLost(cause));
                    }
                }
                Some(command) = self.inbox.recv() => return Some(Next::Command(command)),
                Some(command) = self.piped.next() => return Some(Next::Command(command)),
            }
        }
    }

    /// Drops what was piped and gives the lease up, best-effort.
    async fn end(&mut self) {
        self.unpipe();
        if let Some(lease) = self.lease.take() {
            lease.release().await;
        }
    }

    /// The queued commands: the stash, then the inbox.
    fn drain(&mut self) -> Vec<C> {
        let mut queued: Vec<C> = self.stashed.drain(..).collect();
        queued.extend(std::iter::from_fn(|| self.inbox.try_recv().ok()));
        queued
    }
}

/// Spawns actor `A`'s task on `rt`, stopped when `cancel` is; each start
/// builds it from `args`. Returns its ref.
pub(crate) fn spawn<A: Actor>(
    rt: &tokio::runtime::Handle,
    cancel: CancellationToken,
    args: A::Args,
) -> ActorRef<A::Command> {
    let (inbox, inbox_rx) = mpsc::channel(INBOX_CAPACITY);
    let (system, system_rx) = mpsc::channel(SYSTEM_CAPACITY);
    let cx = Context {
        inbox: inbox_rx,
        system: system_rx,
        stashed: VecDeque::new(),
        piped: FuturesUnordered::new(),
        cancel,
        lease: None,
    };
    rt.spawn(run::<A>(args, cx));
    ActorRef { inbox, system }
}

/// The actor's task: wait for a command, start, serve until stopped, then
/// again. Ends when the runtime is dropped.
async fn run<A: Actor>(args: A::Args, mut cx: Context<A::Command>) {
    loop {
        // Nothing runs until the first command.
        let first = loop {
            match cx.next().await {
                None => return,
                Some(Next::Command(command)) => break command,
                Some(Next::Stopped | Next::LeaseLost(_)) => {}
            }
        };
        let mut actor = match A::start(&args, &mut cx).await {
            Ok(actor) => actor,
            Err(cause) => {
                cx.end().await;
                fail_queued(&mut cx, Some(first), &cause);
                continue;
            }
        };
        let mut command = first;
        let why = loop {
            if let Err(cause) = actor.handle(command, &mut cx).await {
                break Stop::Failed(cause);
            }
            command = match cx.next().await {
                None => {
                    cx.end().await;
                    return;
                }
                Some(Next::Command(command)) => command,
                Some(Next::Stopped) => break Stop::Graceful,
                Some(Next::LeaseLost(cause)) => break Stop::Failed(cause.into()),
            };
        };
        cx.end().await;
        let cause = match &why {
            Stop::Graceful => None,
            Stop::Failed(cause) => Some(cause.clone()),
        };
        actor.stopped(why, &mut cx).await;
        if let Some(cause) = cause {
            fail_queued(&mut cx, None, &cause);
        }
    }
}

/// Fails `first` and every queued command with `cause`. Called once the
/// lease is released, so a caller that retries at once reaches the next start.
fn fail_queued<C: Command>(cx: &mut Context<C>, first: Option<C>, cause: &Error) {
    for command in first.into_iter().chain(cx.drain()) {
        command.fail(cause.clone());
    }
}

/// Waits until `at`; forever when `None`.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
    }
}
