//! An instance's actor lives one incarnation: it ends on a lease or store
//! failure, failing its callers with the cause and releasing its lease, and
//! the next call on the same `AgentInstance` re-attaches a new one.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keelrs::store::{Epoch, Seq, Store, StoreBackend, StoreError, StoredEvent};
use keelrs::testing::ScriptedModel;
use keelrs::{Error, InstanceId};
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};

use crate::support::{TIMEOUT, agent, clock, runtime_on, until_requests};

/// One stream in memory, as the in-memory store keeps it, with failures the
/// test switches on.
#[derive(Clone, Default)]
struct Faulty(Arc<State>);

#[derive(Default)]
struct State {
    stream: Mutex<Stream>,
    appends: AtomicUsize,
    /// The append (counted from 1) that fails as fenced once `gate` opens; 0 for none.
    failing_append: AtomicUsize,
    /// Notified when the failing append is reached.
    reached: Notify,
    gate: Notify,
    renewals_fail: AtomicBool,
}

#[derive(Default)]
struct Stream {
    epoch: u64,
    expires: Option<Instant>,
    events: Vec<StoredEvent>,
}

impl Faulty {
    /// Fails the `n`-th append, once the test calls `fail`.
    fn hold_append(&self, n: usize) {
        self.0.failing_append.store(n, Ordering::SeqCst);
    }

    /// Waits until the held append is reached.
    async fn reached(&self) {
        timeout(TIMEOUT, self.0.reached.notified())
            .await
            .expect("the held append is reached");
    }

    /// Lets the held append fail.
    fn fail(&self) {
        self.0.gate.notify_one();
    }

    fn renewals_fail(&self, fail: bool) {
        self.0.renewals_fail.store(fail, Ordering::SeqCst);
    }
}

fn fenced(agent: &str, id: &InstanceId) -> StoreError {
    StoreError::Fenced {
        agent: agent.to_owned(),
        id: id.clone(),
    }
}

#[keelrs::async_trait]
impl StoreBackend for Faulty {
    async fn acquire(
        &self,
        agent: &str,
        id: &InstanceId,
        ttl: Duration,
    ) -> Result<Epoch, StoreError> {
        let now = Instant::now();
        let mut stream = self.0.stream.lock().unwrap();
        if stream.expires.is_some_and(|expires| expires > now) {
            return Err(StoreError::Held {
                agent: agent.to_owned(),
                id: id.clone(),
            });
        }
        stream.epoch += 1;
        stream.expires = Some(now + ttl);
        Ok(Epoch::new(stream.epoch))
    }

    async fn renew(
        &self,
        agent: &str,
        id: &InstanceId,
        epoch: Epoch,
        ttl: Duration,
    ) -> Result<(), StoreError> {
        let mut stream = self.0.stream.lock().unwrap();
        if self.0.renewals_fail.load(Ordering::SeqCst)
            || stream.epoch != epoch.get()
            || stream.expires.is_none()
        {
            return Err(fenced(agent, id));
        }
        stream.expires = Some(Instant::now() + ttl);
        Ok(())
    }

    async fn release(&self, _: &str, _: &InstanceId, epoch: Epoch) -> Result<(), StoreError> {
        // Takes a while, as over a network.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut stream = self.0.stream.lock().unwrap();
        if stream.epoch == epoch.get() {
            stream.expires = None;
        }
        Ok(())
    }

    async fn read(
        &self,
        _: &str,
        _: &InstanceId,
        after: Seq,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let stream = self.0.stream.lock().unwrap();
        Ok(stream
            .events
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect())
    }

    async fn append(
        &self,
        agent: &str,
        id: &InstanceId,
        epoch: Epoch,
        expected: Seq,
        events: &[StoredEvent],
    ) -> Result<(), StoreError> {
        let n = self.0.appends.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.0.failing_append.load(Ordering::SeqCst) {
            self.0.reached.notify_one();
            self.0.gate.notified().await;
            return Err(fenced(agent, id));
        }
        let mut stream = self.0.stream.lock().unwrap();
        let last = stream.events.last().map_or(Seq::default(), |e| e.seq);
        if stream.epoch != epoch.get() || last != expected {
            return Err(fenced(agent, id));
        }
        stream.events.extend_from_slice(events);
        Ok(())
    }
}

fn is_fenced<T>(result: &keelrs::Result<T>) -> bool {
    matches!(result, Err(Error::Store(StoreError::Fenced { .. })))
}

#[tokio::test]
async fn a_failed_append_fails_the_waiting_callers_and_the_next_call_re_attaches() {
    let backend = Faulty::default();
    let store = Store::new(backend.clone());
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let model = ScriptedModel::new()
        .calls([("c1", "current_time", json!({}))])
        .reply("One again.")
        .reply("Three.");
    let runtime = runtime_on(&store, &agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    // The second append stores the first step: it waits, then fails.
    backend.hold_append(2);
    let one = instance.dispatch("One").await.unwrap();
    backend.reached().await;
    let read = tokio::spawn({
        let instance = instance.clone();
        let one = one.clone();
        async move { instance.read(&one).await }
    });
    let two = tokio::spawn({
        let instance = instance.clone();
        async move { instance.dispatch("Two").await }
    });
    // Both wait on the actor, parked or queued.
    tokio::time::sleep(Duration::from_millis(50)).await;
    backend.fail();

    let read = timeout(TIMEOUT, read).await.unwrap().unwrap();
    assert!(is_fenced(&read), "got {read:?}");
    let two = timeout(TIMEOUT, two).await.unwrap().unwrap();
    assert!(is_fenced(&two), "got {two:?}");

    // A new incarnation recovers the stream: "One" never started, so it runs again.
    let reply = timeout(TIMEOUT, instance.ask("Three")).await.unwrap();
    assert_eq!(reply.unwrap().text(), "Three.");
    let reply = instance.read(&one).await;
    assert_eq!(reply.unwrap().text(), "One again.");
}

#[tokio::test]
async fn a_failed_renewal_ends_the_actor_and_releases_its_lease() {
    let backend = Faulty::default();
    let store = Store::new(backend.clone()).lease_ttl(Duration::from_millis(300));
    let agent = agent(|_| {}).await;
    let model = ScriptedModel::new().hang();
    let runtime = runtime_on(&store, &agent, &model);
    let instance = runtime
        .instance(&agent, InstanceId::new("renewed"))
        .unwrap();

    let one = instance.dispatch("One").await.unwrap();
    until_requests(&model, 1).await;
    backend.renewals_fail(true);
    let read = timeout(TIMEOUT, instance.read(&one)).await.unwrap();
    assert!(is_fenced(&read), "got {read:?}");

    // Released: a second runtime takes the lease at once, without waiting for the TTL.
    backend.renewals_fail(false);
    let model = ScriptedModel::new().reply("One again.");
    let second = runtime_on(&store, &agent, &model);
    let other = second.instance(&agent, InstanceId::new("renewed")).unwrap();
    let reply = timeout(TIMEOUT, other.read(&one)).await.unwrap();
    assert_eq!(reply.unwrap().text(), "One again.");
}

#[tokio::test]
async fn asking_for_an_instance_takes_no_lease() {
    let store = Store::in_memory();
    let agent = agent(|_| {}).await;
    let id = InstanceId::new("lazy");
    let idle = runtime_on(&store, &agent, &ScriptedModel::new());
    let _unused = idle.instance(&agent, id.clone()).unwrap();

    let model = ScriptedModel::new().reply("Hi.");
    let runtime = runtime_on(&store, &agent, &model);
    let instance = runtime.instance(&agent, id).unwrap();
    let reply = timeout(TIMEOUT, instance.ask("Hello")).await.unwrap();
    assert_eq!(reply.unwrap().text(), "Hi.");
}

#[tokio::test]
async fn calls_sent_while_the_actor_starts_are_served_in_order() {
    let agent = agent(|_| {}).await;
    let model = ScriptedModel::new().reply("A.").reply("B.").reply("C.");
    let runtime = runtime_on(&Store::in_memory(), &agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    let (a, b, c) = tokio::join!(instance.ask("A"), instance.ask("B"), instance.ask("C"));
    let texts = [a, b, c].map(|reply| reply.unwrap().text().to_owned());
    assert_eq!(texts, ["A.", "B.", "C."]);
}

#[tokio::test]
async fn a_held_instance_is_unavailable_once_its_runtime_drops() {
    let agent = agent(|_| {}).await;
    let model = ScriptedModel::new().reply("Hi.");
    let runtime = runtime_on(&Store::in_memory(), &agent, &model);
    let instance = runtime.instance(&agent, InstanceId::new("gone")).unwrap();
    timeout(TIMEOUT, instance.ask("Hello"))
        .await
        .unwrap()
        .unwrap();
    drop(runtime);

    let dispatched = instance.dispatch("Hello").await;
    assert!(
        matches!(dispatched, Err(Error::InstanceUnavailable { .. })),
        "got {dispatched:?}"
    );
    let aborted = instance.abort().await;
    assert!(
        matches!(aborted, Err(Error::InstanceUnavailable { .. })),
        "got {aborted:?}"
    );
}

#[tokio::test]
async fn a_call_right_after_a_failed_dispatch_re_attaches() {
    let backend = Faulty::default();
    let store = Store::new(backend.clone());
    let agent = agent(|_| {}).await;
    let model = ScriptedModel::new().reply("Two.");
    let runtime = runtime_on(&store, &agent, &model);
    let instance = runtime.instance(&agent, InstanceId::fresh()).unwrap();

    // The first append stores "One": it fails as soon as it is reached.
    backend.hold_append(1);
    backend.fail();
    let one = instance.dispatch("One").await;
    assert!(is_fenced(&one), "got {one:?}");
    let reply = timeout(TIMEOUT, instance.ask("Two")).await.unwrap();
    assert_eq!(reply.unwrap().text(), "Two.");
}
