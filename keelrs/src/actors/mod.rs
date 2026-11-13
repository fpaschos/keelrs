//! The keelrs actor pattern: Alice Ryhl's "Actors with Tokio", with a few
//! Akka ideas. Not a framework: each actor writes its own loop, and the
//! library never calls into it.
//!
//! Three layers, each opt-in over the one before:
//!
//! - **core** (this file): an [`ActorRef`] to send to, and the [`Mailbox`]
//!   the actor's task reads: the system lane (the library's own messages),
//!   then the inbox (the actor's commands), then what the actor piped to
//!   itself. One task per actor; the actor restarts in place inside it, so
//!   its `ActorRef` never changes.
//! - **keyed** ([`entity`]): [`Entities`](entity::Entities) holds every actor
//!   of one type by key.
//! - **persistent** ([`lease`]): a [`Lease`] the mailbox holds makes the actor
//!   the single writer of its stream.

pub(crate) mod entity;
pub(crate) mod lease;

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

/// How many commands an inbox holds before senders wait.
const INBOX_CAPACITY: usize = 1024;

/// How many system messages wait; a `Stop` already waiting is enough.
const SYSTEM_CAPACITY: usize = 1;

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

    /// Ends the current incarnation at once; writes nothing. The inbox keeps
    /// its commands, for the next incarnation.
    pub(crate) fn stop(&self) {
        // Full: a `Stop` is already waiting.
        let _ = self.system.try_send(System::Stop);
    }
}

/// The library's own messages: written only by the library, served before
/// the inbox.
enum System {
    Stop,
}

/// What an actor's task reads: the system lane, its inbox, what it piped to
/// itself, and the token that stops it; and the lease it holds, if any.
pub(crate) struct Mailbox<C> {
    inbox: mpsc::Receiver<C>,
    system: mpsc::Receiver<System>,
    piped: FuturesUnordered<BoxFuture<'static, C>>,
    cancel: CancellationToken,
    lease: Option<Lease>,
}

/// What [`Mailbox::next`] yields.
pub(crate) enum Next<C> {
    Command(C),
    /// [`ActorRef::stop`] ended the incarnation.
    Stopped,
    /// The held lease could not be renewed: the incarnation ended.
    Ended(StoreError),
}

impl<C> Mailbox<C> {
    /// The next thing to serve, in order: cancel (`None`), the system lane,
    /// the held lease's renewal, the inbox, then what was piped.
    pub(crate) async fn next(&mut self) -> Option<Next<C>> {
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
                        return Some(Next::Ended(cause));
                    }
                }
                Some(command) = self.inbox.recv() => return Some(Next::Command(command)),
                Some(command) = self.piped.next() => return Some(Next::Command(command)),
            }
        }
    }

    /// Runs `future` in the actor's task, as Akka's `pipeToSelf`: its output
    /// comes back as a command once the inbox is empty. Polled only from
    /// [`next`](Self::next), so it waits while a command is served.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "a write to the mailbox; `&mut` keeps `Mailbox` free of a `Sync` bound"
    )]
    pub(crate) fn pipe_to_self(&mut self, future: impl Future<Output = C> + Send + 'static) {
        self.piped.push(Box::pin(future));
    }

    /// Drops every future piped so far: they stop at once.
    pub(crate) fn unpipe(&mut self) {
        self.piped.clear();
    }

    /// Holds `lease`: renewed from [`next`](Self::next) from now on.
    pub(crate) fn hold(&mut self, lease: Lease) {
        self.lease = Some(lease);
    }

    /// The deadline guard: [`StoreError::Fenced`] unless the held lease is
    /// still trusted.
    pub(crate) fn guard(&self) -> Result<(), StoreError> {
        self.lease.as_ref().map_or(Ok(()), Lease::guard)
    }

    /// Ends the incarnation: drops what it piped and gives its lease up,
    /// best-effort.
    pub(crate) async fn end(&mut self) {
        self.unpipe();
        if let Some(lease) = self.lease.take() {
            lease.release().await;
        }
    }

    /// The queued commands, to answer at an end.
    pub(crate) fn drain(&mut self) -> Vec<C> {
        std::iter::from_fn(|| self.inbox.try_recv().ok()).collect()
    }
}

/// Spawns an actor's task `run` on `rt`, stopped when `cancel` is; returns
/// its ref.
pub(crate) fn spawn<C, F>(
    rt: &tokio::runtime::Handle,
    cancel: CancellationToken,
    run: impl FnOnce(Mailbox<C>) -> F,
) -> ActorRef<C>
where
    C: Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let (inbox, inbox_rx) = mpsc::channel(INBOX_CAPACITY);
    let (system, system_rx) = mpsc::channel(SYSTEM_CAPACITY);
    rt.spawn(run(Mailbox {
        inbox: inbox_rx,
        system: system_rx,
        piped: FuturesUnordered::new(),
        cancel,
        lease: None,
    }));
    ActorRef { inbox, system }
}

/// Waits until `at`; forever when `None`.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
    }
}
