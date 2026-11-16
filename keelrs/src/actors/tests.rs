//! How layer 1 is used: piping a future to self, and building each start.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{Actor, ActorRef, Context, PipeGuard, setup, spawn, spawn_with};
use crate::{Error, Result};

/// What the probe does with a piped future's guard.
#[derive(Clone, Copy)]
enum Guard {
    Drop,
    Detach,
}

enum Probe {
    /// Pipes `future` and handles its guard as `guard` says.
    Pipe {
        future: BoxFuture<'static, Self>,
        guard: Guard,
        respond_to: oneshot::Sender<()>,
    },
    /// A piped future came back.
    Done(oneshot::Sender<()>),
    /// Answers with the start that built the actor.
    Start(oneshot::Sender<usize>),
}

/// A probe actor, built by its `start`-th start.
struct Prober {
    start: usize,
}

impl Actor for Prober {
    type Command = Probe;
    type Args = ();

    async fn start((): (), _: &mut Context<Probe>) -> Result<Self> {
        Ok(Self { start: 1 })
    }

    async fn handle(&mut self, command: Probe, cx: &mut Context<Probe>) -> Result<()> {
        match command {
            Probe::Pipe {
                future,
                guard,
                respond_to,
            } => {
                let piped: PipeGuard = cx.pipe_to_self(future);
                match guard {
                    Guard::Drop => drop(piped),
                    Guard::Detach => piped.detach(),
                }
                let _ = respond_to.send(());
            }
            Probe::Done(respond_to) => {
                let _ = respond_to.send(());
            }
            Probe::Start(respond_to) => {
                let _ = respond_to.send(self.start);
            }
        }
        Ok(())
    }

    async fn post_stop(self, _: Option<&Error>, _: &mut Context<Probe>) {}
}

fn prober() -> ActorRef<Probe> {
    spawn::<Prober>(&Handle::current(), CancellationToken::new(), ())
}

/// Pipes a future that comes back as `Done` once `gate` opens; the receiver
/// gets `Ok` only if the actor served `Done`.
async fn pipe(
    r: &ActorRef<Probe>,
    guard: Guard,
    gate: oneshot::Receiver<()>,
) -> oneshot::Receiver<()> {
    let (done, served) = oneshot::channel();
    let future = Box::pin(async move {
        let _ = gate.await;
        Probe::Done(done)
    });
    r.ask(|respond_to| Probe::Pipe {
        future,
        guard,
        respond_to,
    })
    .await
    .expect("the actor answers");
    served
}

#[tokio::test]
async fn a_dropped_guard_cancels_its_future() {
    let r = prober();
    let (open, gate) = oneshot::channel();
    let served = pipe(&r, Guard::Drop, gate).await;
    let _ = open.send(());
    assert!(served.await.is_err(), "the future came back");
}

#[tokio::test]
async fn a_detached_future_runs_to_its_end_and_comes_back() {
    let r = prober();
    let (open, gate) = oneshot::channel();
    let served = pipe(&r, Guard::Detach, gate).await;
    let _ = open.send(());
    assert!(served.await.is_ok(), "the future did not come back");
}

#[tokio::test]
async fn a_stop_drops_detached_futures_too() {
    let r = prober();
    let (_open, gate) = oneshot::channel();
    let served = pipe(&r, Guard::Detach, gate).await;
    r.stop();
    assert!(served.await.is_err(), "the future outlived the stop");
}

#[tokio::test]
async fn spawn_with_builds_each_start_from_its_factory() {
    let starts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&starts);
    let r = spawn_with::<Prober>(
        &Handle::current(),
        CancellationToken::new(),
        setup(move |_| {
            let start = counted.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move { Ok(Prober { start }) })
        }),
    );
    let start = |r: ActorRef<Probe>| async move {
        // A stop drops what is queued: ask until a start answers.
        loop {
            if let Some(start) = r.ask(Probe::Start).await {
                return start;
            }
        }
    };
    assert_eq!(start(r.clone()).await, 1);
    r.stop();
    assert_eq!(start(r.clone()).await, 2);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
}
