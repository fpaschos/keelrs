//! The keelrs actor pattern: Alice Ryhl's "Actors with Tokio", with a few
//! Akka ideas. The library owns the loop; an actor gives its hooks
//! ([`Actor::start`], [`Actor::handle`], [`Actor::post_stop`]).
//!
//! keelrs has three layers, each knowing only the one below it:
//!
//! 1. **this library**: an [`ActorRef`] to send to, and one task per actor
//!    that runs it; [`entity`] holds every actor of one type by key. It knows
//!    no agent, store, lease or event, and grows only when a core actor needs
//!    a feature.
//! 2. **the core actors**, built on it: the agent instance actor owns its
//!    store, lease, fold and queue.
//! 3. **what they run**: plain futures, piped to the actor
//!    ([`Context::pipe_to_self`]), that know no actor.
//!
//! An actor's lifecycle, Akka's default: spawn → idle → [`Actor::start`] on
//! the first command → [`Actor::handle`] each command → stop (after the
//! current command) → the library drops what was piped and scheduled, and the
//! inbox → [`Actor::post_stop`] → idle, or the end on shutdown. The next
//! command starts the actor again behind the same [`ActorRef`].

pub(crate) mod entity;

#[cfg(test)]
mod tests;

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::{AbortHandle, Abortable, BoxFuture};
use futures::stream::FuturesUnordered;
use tokio::runtime::Handle;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

/// How many commands an inbox holds before senders wait.
const INBOX_CAPACITY: usize = 1024;

/// How many system messages wait; a `Stop` already waiting is enough.
const SYSTEM_CAPACITY: usize = 1;

/// An actor: its commands, the arguments each start builds it from, and its
/// hooks. The library calls them from one task, one at a time.
// Bridge until return type notation is stable (rust-lang/goals#646): then
// plain `async fn` hooks with `where A::handle(..): Send` in `spawn`.
#[trait_variant::make(Send)]
pub(crate) trait Actor: Sized + Send + 'static {
    type Command: Send + 'static;
    /// What each start builds the actor from; fixed for the `ActorRef`'s life.
    type Args: Clone + Send + Sync + 'static;

    /// Builds the actor, on the first command after spawn or after a stop
    /// (lazy start). Commands sent meanwhile wait in the inbox, in order.
    async fn start(args: Self::Args, cx: &mut Context<Self::Command>) -> Result<Self>;

    /// Serves one command. An error stops the actor with that cause.
    async fn handle(
        &mut self,
        command: Self::Command,
        cx: &mut Context<Self::Command>,
    ) -> Result<()>;

    /// Akka `PostStop`. `Some(cause)`: a failure; `None`: a requested stop or
    /// a shutdown. The actor's fields drop when it returns.
    async fn post_stop(self, cause: Option<&Error>, cx: &mut Context<Self::Command>);

    /// Lazy start only: `start` failed on `first`, the command that triggered
    /// it. Default: drop it (its caller gets `None`). The agent instance
    /// answers with the cause, because `Held` is how a replica learns that
    /// another one owns the instance (sharding by lease). Not an Akka hook:
    /// Akka starts eagerly.
    fn start_failed(first: Self::Command, cause: &Error) {
        let _ = (first, cause);
    }
}

/// What builds the actor at each start, Akka `Behaviors.setup` style.
pub(crate) type Setup<A> = Arc<
    dyn for<'a> Fn(&'a mut Context<<A as Actor>::Command>) -> BoxFuture<'a, Result<A>>
        + Send
        + Sync,
>;

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
    /// actor answered (the runtime was dropped, or the actor stopped first).
    pub(crate) async fn ask<R>(&self, make: impl FnOnce(oneshot::Sender<R>) -> C) -> Option<R> {
        let (respond_to, answer) = oneshot::channel();
        self.inbox.send(make(respond_to)).await.ok()?;
        answer.await.ok()
    }

    /// Stops the actor after its current command; the queued ones are dropped.
    #[cfg_attr(not(test), expect(dead_code, reason = "kept for passivation"))]
    pub(crate) fn stop(&self) {
        // Full: a `Stop` is already waiting.
        let _ = self.system.try_send(System::Stop);
    }
}

/// The library's own messages, served before the inbox.
enum System {
    #[cfg_attr(not(test), expect(dead_code, reason = "kept for passivation"))]
    Stop,
}

/// Owns a future piped with [`Context::pipe_to_self`]: dropping it cancels
/// the future.
#[must_use = "dropping a PipeGuard cancels its future; call .detach() to let it run"]
pub(crate) struct PipeGuard(Option<AbortHandle>);

impl PipeGuard {
    /// Lets the future run to its end; only a stop drops it.
    #[cfg_attr(not(test), expect(dead_code, reason = "no core actor detaches yet"))]
    pub(crate) fn detach(mut self) {
        // Not `mem::forget`: that would leak the handle's `Arc`.
        self.0 = None;
    }
}

impl Drop for PipeGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// What an actor's hooks are given: its mailbox, as far as an actor may
/// touch it.
pub(crate) struct Context<C> {
    inbox: mpsc::Receiver<C>,
    system: mpsc::Receiver<System>,
    /// `None`: the future was cancelled, and is skipped.
    piped: FuturesUnordered<BoxFuture<'static, Option<C>>>,
    /// The one scheduled command, and when it is due.
    timer: Option<(Instant, C)>,
    cancel: CancellationToken,
    self_ref: ActorRef<C>,
}

/// What [`Context::next`] yields.
enum Next<C> {
    Command(C),
    Stopped,
}

impl<C> Context<C> {
    /// This actor's own ref (Akka `context.self`).
    pub(crate) fn self_ref(&self) -> ActorRef<C> {
        self.self_ref.clone()
    }

    /// Runs `future` in the actor's task (Akka `pipeToSelf`); its output comes
    /// back as a command once the inbox is empty. Dropping the guard cancels it.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "a write to the mailbox; `&mut` keeps `Context` free of a `Sync` bound"
    )]
    pub(crate) fn pipe_to_self(
        &mut self,
        future: impl Future<Output = C> + Send + 'static,
    ) -> PipeGuard {
        let (handle, registration) = AbortHandle::new_pair();
        let future = Abortable::new(future, registration);
        self.piped.push(Box::pin(async move { future.await.ok() }));
        PipeGuard(Some(handle))
    }

    /// Sends `command` to the actor at `at`, before the inbox; replaces the
    /// command scheduled before. Dropped when the actor stops.
    pub(crate) fn schedule(&mut self, at: Instant, command: C) {
        self.timer = Some((at, command));
    }

    /// The next thing to serve, in order: cancel (`None`), the system lane,
    /// the scheduled command once due, the inbox, then what was piped.
    async fn next(&mut self) -> Option<Next<C>> {
        loop {
            let due = self.timer.as_ref().map(|(at, _)| *at);
            let piped = select! {
                biased;
                () = self.cancel.cancelled() => return None,
                Some(System::Stop) = self.system.recv() => return Some(Next::Stopped),
                () = time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                    return self.timer.take().map(|(_, command)| Next::Command(command));
                }
                Some(command) = self.inbox.recv() => return Some(Next::Command(command)),
                Some(piped) = self.piped.next() => piped,
            };
            if let Some(command) = piped {
                return Some(Next::Command(command));
            }
        }
    }

    /// The next command, a stop ignored: the actor is idle.
    async fn next_command(&mut self) -> Option<C> {
        loop {
            if let Next::Command(command) = self.next().await? {
                return Some(command);
            }
        }
    }

    /// Drops what was piped and scheduled, and the queued commands: their
    /// callers get no answer.
    fn clear(&mut self) {
        self.piped.clear();
        self.timer = None;
        while self.inbox.try_recv().is_ok() {}
    }
}

/// Spawns actor `A`'s task on `rt`, stopped when `cancel` is; each start
/// builds it with `setup`. The primitive; [`spawn`] is built on it.
pub(crate) fn spawn_with<A: Actor>(
    rt: &Handle,
    cancel: CancellationToken,
    setup: Setup<A>,
) -> ActorRef<A::Command> {
    let (inbox, inbox_rx) = mpsc::channel(INBOX_CAPACITY);
    let (system, system_rx) = mpsc::channel(SYSTEM_CAPACITY);
    let r = ActorRef { inbox, system };
    let cx = Context {
        inbox: inbox_rx,
        system: system_rx,
        piped: FuturesUnordered::new(),
        timer: None,
        cancel,
        self_ref: r.clone(),
    };
    rt.spawn(run(setup, cx));
    r
}

/// Spawns actor `A`'s task on `rt`, stopped when `cancel` is; each start
/// builds it from `args`.
pub(crate) fn spawn<A: Actor>(
    rt: &Handle,
    cancel: CancellationToken,
    args: A::Args,
) -> ActorRef<A::Command> {
    spawn_with(
        rt,
        cancel,
        setup(move |cx| Box::pin(A::start(args.clone(), cx))),
    )
}

/// `f` as a [`Setup`]; names the closure's signature for the compiler.
pub(crate) fn setup<A: Actor>(
    f: impl for<'a> Fn(&'a mut Context<A::Command>) -> BoxFuture<'a, Result<A>> + Send + Sync + 'static,
) -> Setup<A> {
    Arc::new(f)
}

/// The actor's task: wait for a command, start, serve until stopped, then
/// again. Ends when the runtime is dropped.
///
/// A stop drops the inbox before the hooks answer anyone, so a caller that
/// retries once answered reaches the next start.
async fn run<A: Actor>(setup: Setup<A>, mut cx: Context<A::Command>) {
    // Nothing runs until the first command.
    while let Some(first) = cx.next_command().await {
        let mut actor = match setup(&mut cx).await {
            Ok(actor) => actor,
            Err(cause) => {
                cx.clear();
                A::start_failed(first, &cause);
                continue;
            }
        };
        let mut command = first;
        let (cause, shutdown) = loop {
            if let Err(cause) = actor.handle(command, &mut cx).await {
                break (Some(cause), false);
            }
            command = match cx.next().await {
                None => break (None, true),
                Some(Next::Command(command)) => command,
                Some(Next::Stopped) => break (None, false),
            };
        };
        cx.clear();
        actor.post_stop(cause.as_ref(), &mut cx).await;
        if shutdown {
            return;
        }
    }
}
