# Durable actors: Akka Persistence, virtual actors, Rust options, and an event-sourced `InstanceActor`

Research for keelrs (a Rust agent framework built on Rig). An agent instance today is a tokio actor in Alice Ryhl's "Actors with Tokio" style [S30]. Its canonical events are meant to be the source of truth (ADR 0004), and agents will be durable in SQL via sqlx. This note asks what a **durable actor** (Akka's persistent actor) is, how virtual-actor platforms get the same properties, which Rust crates help, and how `InstanceActor` should change.

Researched 2026-11-04. Docs claims cite `[S#]`, keyed to the Sources list at the end. Code claims cite `repo@commit:path:line`. Claims marked **[inference]** are my reading, not a statement any source makes.

Read first; not repeated here: `docs/research/event-sourced-harnesses.md` (branch `research/event-sourced-harnesses`). It covers intent-before-effect, expected-seq appends, writer epochs, the Restate journal, Durable Streams and the envelope fields. This note adds the **actor lifecycle** around those rules.

---

## 1. Akka Persistence (Typed): the reference model

Akka's docs [S1–S5] and Apache Pekko's fork [S6, Pekko 1.7.1] describe the same model. Pekko's page repeats Akka's wording ("Pekko Persistence is based on the single-writer principle"; "will always receive a RecoveryCompleted signal, even if there are no events in the journal") [S6].

### `EventSourcedBehavior`: command, effect, event, state

An `EventSourcedBehavior` is built from four parts [S1]:
- `persistenceId`: "the stable unique identifier for the persistent actor in the backend event journal and snapshot store".
- `emptyState`: "the `State` when the entity is first created".
- `commandHandler`: "defines how to handle command by producing Effects e.g. persisting events, stopping the persistent actor".
- `eventHandler`: "returns the new state given the current state when an event has been persisted".

The command handler does not change state. It returns an **Effect** [S1]:
- `persist`: "will persist one single event or several events atomically".
- `none` (a read-only command), `unhandled`, `stop`.
- `stash` and `unstashAll`.
- `reply` / `thenReply`, and `thenRun`: "All `thenRun` registered callbacks are executed sequentially after successful execution of the persist statement".

So the cycle is: command → effect → persisted event(s) → event handler → new state → side effects and reply. "The event handler must only update the state and never perform side effects"; "Side effects should be performed in `thenRun` from the command handler" [S1]. Replies have their own effect because "you typically want to know if the command was rejected due to validation errors" [S1].

### `persistenceId`, journal, sequence numbers

- Every event is stored in the journal under the `persistenceId` with a per-id sequence number [S1]. Recovery always restores the highest sequence number "so you can keep persisting new events without corrupting your event log" [S1].
- The failure the docs warn about is two writers that "journaled different messages with the same sequence number" [S1] (the replay-filter section).

### Recovery and `RecoveryCompleted`

- "An event sourced actor is automatically recovered on start and on restart by replaying journaled events" [S1].
- "The actor will always receive a `RecoveryCompleted` signal" [S1, S6]. That signal carries the recovered state and is the place for end-of-recovery side effects. Inspecting state there "may possibly result in executing side effects more than once" [S1].
- "It is strongly discouraged to perform side effects in the event handler", because the event handler also runs during replay [S1].

### Snapshots and retention

- Snapshots are taken "every N events or when a given predicate of the state is fulfilled" (`snapshotWhen`, `withRetention(RetentionCriteria.snapshotEvery(n, keepN))`) [S2].
- "During recovery, the persistent actor is using the latest saved snapshot to initialize the state", then replays the events after it [S2].
- Deleting events on snapshot is opt-in (`withDeleteEventsOnSnapshot`, "disabled by default") [S2].
- A failed snapshot save emits `SnapshotFailed` and does "not cause the actor to stop or restart". A failed snapshot load during recovery stops the actor, unless snapshot loading is made optional [S2].

### Stashing while persisting or recovering

- "Incoming messages are stashed automatically until the `persist` is completed" [S1].
- "Commands are also stashed during recovery and will not interfere with replayed events" [S1].
- The stash is bounded: `akka.persistence.typed.stash-capacity = 10000`, per entity `withStashCapacity`. When it is full, "the commands will be dropped" [S1]. Stashed commands are discarded if the actor is restarted or stopped by an exception [S1].

### Single writer, and what breaks it

- "For a particular `persistenceId` only one persistent actor instance should be active at one time." Otherwise "the events would be interleaved and might not be interpreted correctly on replay" [S1].
- Two incarnations break it. The usual cause is starting the same id on two nodes, or a restart while the old one still runs.
- The fix is Cluster Sharding: "Cluster Sharding ensures that there is only one active entity for each id" [S1]. "Each entity actor runs only at one place", and sharding "is typically used together with persistence to ensure that there is only one active entity" [S3].
- **[inference]** Akka relies on placement (sharding plus a split-brain resolver) for single writer. The journal's sequence-number constraint is a last line of defence, not the mechanism.

### Passivation

- "If the state of the entities are persistent you may stop entities that are not used to reduce memory consumption" [S3].
- "Idle entities can be automatically passivated when they have not received a message for a specified length of time". The default is on, "with a timeout of 2 minutes" [S3].
- Messages that arrive while an entity is passivating are buffered by the shard and "delivered to a new incarnation of the entity" [S3]. "Without remembered entities restarts happen on the arrival of a message" [S3].
- Remember-entities restarts entities after a rebalance or crash, and "disables Automatic Passivation" [S3].

### Side effects and at-least-once

- "Any side effects are executed on an at-most-once basis and will not be executed if the persist fails" [S1, S6].
- A crash between persist and `thenRun` loses the effect. **[inference]** At-least-once therefore needs the effect's intent in the log, plus a re-run on `RecoveryCompleted`. This is the intent-before-effect rule of the earlier note, and it is why Akka warns about duplicates there.

### Event adapters and schema evolution

- Event adapters "convert from your Event type to another type that is then passed to the journal" [S1]. Upcasting promotes old events "to the latest versions during recovery", and stored events "are immutable and usually never deleted" [S4].

### Persistence query

- Persistence Query is "a universal asynchronous stream based query interface" over the journal [S5].
- `eventsByPersistenceId` is live and keeps watching for new events. `currentEventsByPersistenceId` is finite [S5]. `eventsByTag` crosses ids, with ordering often not guaranteed [S5].
- Offsets are exclusive and make streams resumable. Projections store the last processed offset (Akka Projections) [S5].

---

## 2. Virtual actors and durable objects

The platforms below remove the "who spawns it" question. The id always exists, and the runtime activates it on demand.

### Microsoft Orleans grains (`JournaledGrain`)

- **Identity and lifecycle.** Grains are virtual actors: "Actors are purely logical entities that *always* exist, virtually. An actor cannot be explicitly created nor destroyed" [S7]. "The Orleans runtime automatically instantiates grains on demand. Grains not used for a while are automatically removed from memory" [S7]. Idle collection uses `CollectionAge` (default 15 minutes) [S8].
- **Single activation.** The grain directory maps a grain id to the silo hosting it. Since Orleans 9 it is strongly consistent, and crash recovery explicitly works toward "Ensuring no duplicate grain activations occur" [S9].
- **Turn-based execution.** "By default, they process each request from beginning to completion before the next request can begin processing". Reentrancy is opt-in [S10].
- **Plain persistence.** State "load[s] from storage during grain activation", before `OnActivateAsync`. Writes are explicit (`WriteStateAsync`) and guarded by an ETag. A violation faults with `InconsistentStateException` [S11].
- **Event-sourced variant.** `JournaledGrain<TState, TEvent>` [S12]:
  - `RaiseEvent` "initiates a write to storage but doesn't wait"; `await ConfirmEvents()` waits.
  - `RaiseEvents(IEnumerable)` "guarantees the given sequence of events is written to storage atomically".
  - State changes only through `Apply`/`TransitionState`, which "should be deterministic". `Version` "always equals the total number of confirmed events".
  - `RetrieveConfirmedEvents(from, to)` reads the log.
- **Log-consistency providers** [S13]:
  - `StateStorage` stores a state snapshot plus the version, but no events.
  - `LogStorage` stores the whole event list as one object; it is "*not suitable for production use*".
  - `CustomStorage` is the one closest to keelrs: you implement `ReadStateFromStorage()` → `(version, state)` and `ApplyUpdatesToStorage(updates, expectedVersion)` → `bool`. The second "must return `false` if the expected version doesn't match". Retries after an exception can duplicate events.

### Cloudflare Durable Objects

- **Identity and single instance.** "Each Durable Object has a globally-unique name", is "implicitly created on first access", and is "single-threaded and cooperatively multi-tasked" [S14].
- **Storage.** "Each Durable Object has some durable storage attached" [S14]. A SQLite database is "private to its unique instance". "Any series of write operations with no intervening `await` will automatically be submitted atomically". It supports point-in-time restore over 30 days [S15].
- **Gates (stash and effects).**
  - Input gate: "While a storage operation is executing, no events shall be delivered to the object except for storage completion events" [S16].
  - Output gate: "any new outgoing network messages will be held back until the write has completed" [S16]. This is Akka's "effects after persist", enforced by the runtime.
  - `blockConcurrencyWhile` is "commonly used within the constructor" to load state before requests arrive [S17]. That is recovery plus stash.
- **Hibernation and passivation.** "When hibernated, the in-memory state is discarded". The constructor runs again on the next request. Idle objects are "evicted entirely from memory" after 70–140 s, and "Hibernated WebSocket connections stay connected" [S18].
- The Flue note records that Flue on Cloudflare runs one Durable Object per agent instance.

### Restate virtual objects (beyond the existing note)

- "Stateful entities identified by a unique key". "At most one handler with write access can run at a time per object key. Mimicks a queue per object key". Shared handlers run concurrently and are read-only [S19].
- K/V state is "scoped per object key and retained indefinitely". By default it is "automatically sent with the request when invoking a handler" (eager); lazy state is fetched on `get` [S20].
- State updates are "recorded alongside execution steps for consistency", and on failure "Restate replays the journal" [S21].
- **[inference]** Restate has no resident in-memory actor to passivate. Every invocation brings its state, and the per-key queue is the stash.

### Dapr actors

- Activated on first request; the runtime "garbage-collects the in-memory object of unused actors". The idle timeout and scan interval are configurable [S22, S23].
- "A single actor instance cannot process more than one request at a time", under a "turn-based access model" [S23]. The placement service routes calls and migrates actors to healthy nodes [S22].
- State lives in a state store. A transactional store is required (`actorStateStore: true`) [S22].
- Timers are lost on deactivation. Reminders persist through the state provider [S22].
- Dapr actors are state-based, not event-sourced.

---

## 3. Comparison

| Concept | Akka Persistence | Orleans (`JournaledGrain`) | Durable Objects | Restate virtual object | Dapr actor | keelrs today |
|---|---|---|---|---|---|---|
| **Identity** | `persistenceId` [S1] | grain key; always exists [S7] | globally unique name [S14] | object key [S19] | actor id, activated on first call [S22] | `(agent name, InstanceId)` key in `Runtime.instances` (`runtime.rs:29`) |
| **Single writer** | one active per id; Cluster Sharding enforces it [S1, S3] | single activation via grain directory; turn-based [S9, S10] | one instance per name, single-threaded [S14] | one exclusive handler per key [S19] | turn-based, placement service [S22, S23] | one tokio task per key, **in one process only**; no cross-process guard |
| **Persist** | `Effect.persist` events atomically [S1] | `RaiseEvent(s)` + `ConfirmEvents`; `expectedVersion` [S12, S13] | SQLite/KV writes; implicit atomic batches [S15] | K/V state + journal [S20, S21] | state store (transactional) [S22] | nothing: `STORE:` markers (`instance.rs:185, 219, 245`) |
| **Recover** | replay → `RecoveryCompleted` [S1] | state loaded before `OnActivateAsync` [S11]; `LogStorage` replays [S12] | constructor reruns; `blockConcurrencyWhile` [S17, S18] | replay journal; state sent eagerly [S20, S21] | state loaded from store on activation [S23] | none: history starts empty (`instance.rs:220`) |
| **Snapshot** | `snapshotWhen` / retention [S2] | `StateStorage` provider [S13] | n/a (storage is the state) | n/a (state is K/V) | n/a | none |
| **Stash** | automatic during persist and recovery; capacity 10 000, then drop [S1] | requests queue (non-reentrant) [S10] | input gates [S16] | per-key queue [S19] | turn-based queue [S23] | the mpsc inbox (1024, sender waits) (`instance.rs:28`) |
| **Passivate** | idle 2 min under sharding; buffered and re-spawned [S3] | idle collection, 15 min [S7, S8] | hibernation / eviction 70–140 s [S18] | n/a [inference] | idle GC [S23] | never: actors live until the `Runtime` drops (`runtime.rs:116-120`) |
| **Effects** | `thenRun`, at-most-once after persist [S1] | after `await ConfirmEvents` [S12] [inference] | output gates hold messages until durable [S16] | `ctx.run` journaled [existing note] | none special | the model call runs, then events would be stored (`instance.rs:222-246`) |
| **Query / subscribe** | Persistence Query, live and current streams, offsets [S5] | `RetrieveConfirmedEvents` [S12]; Orleans streams [S7] | hibernatable WebSockets [S18] | pubsub virtual object [existing note] | none built in [inference] | `read(receipt)` from an in-memory map only (`instance.rs:251-260`) |

keelrs paths are at `fpaschos/keelrs@29b20b5:keelrs/src/...`.

---

## 4. Rust options

Versions and dates come from the crates.io API on 2026-11-04 [S24]. "Head" is the default-branch commit read on the same day.

### Actor crates

| Crate | Latest | Maintenance | Persistence / ES built in | Verdict |
|---|---|---|---|---|
| `ractor` | 0.16.5 (2026-08-07) | active; head `slawlor/ractor@1af0aaa` (2026-08-17) | **No**. Erlang `gen_server` model, supervision, registry, process groups (`ractor@1af0aaa:README.md:42-53`) | Don't depend. keelrs needs one actor kind with a store, not supervision trees. |
| `ractor_cluster` | 0.16.5 | same repo | No; remote actors only. "shouldn't be considered production ready" (`ractor@1af0aaa:README.md:36`) | Don't depend. The multi-process route should be fencing over SQL, not a node mesh. |
| `kameo` | 0.22.2 (2026-07-18) | active; head `tqwewe/kameo@1b6bffb` (2026-09-13) | **No**. Supervision; remote actors over libp2p (`kameo@1b6bffb:README.md:126`). A third-party `kameo-persistence` 0.1.0 exists (2025-07, tiny usage) [S24] | Don't depend. |
| `actix` | 0.13.5 (2024-06-09) | repo active (`actix/actix@8e04016`, 2026-11-03), crate unreleased since 2024 | **No**. Features list: async/sync actors, local context, supervision (`actix@8e04016:actix/README.md:31-38`) | Don't depend. Its own runtime context; no fit. |
| `coerce` | 0.8.11 (2023-10-16) | **stale**: last commit `LeonHartley/Coerce-rs@a2d3f19` (2024-02-16) | **Yes**: `PersistentActor` with `persist`, `persist_batch`, `snapshot`, `recover`, recovery-failure policy (`Coerce-rs@a2d3f19:coerce/src/persistent/actor.rs:18-154`). `JournalStorage` trait keyed by `persistence_id` and `sequence` (`.../journal/storage.rs:14-51`). Only an in-memory and a Redis provider (`README.md:11`). Has sharding (`README.md:47-50`) | Don't depend (unmaintained, no SQL). **Copy the shape**: the `PersistentActor` hooks and the `JournalStorage` split of messages and snapshots. |

Alice Ryhl's pattern [S30] (a handle holding an `mpsc::Sender`, a task owning the receiver and the state, replies over `oneshot`) is what `AgentInstance` already is. **[inference]** None of the actor crates adds what keelrs lacks: journal, recovery, passivation, fencing. Each would add a second actor vocabulary. Keep the keelrs-owned pattern (#48) and add persistence to it.

### Event-sourcing crates

| Crate | Latest | Maintenance | Model | sqlx store | Verdict |
|---|---|---|---|---|---|
| `cqrs-es` | 0.5.0 (2025-12-30) | active; `serverlesstechnology/cqrs@37c1ab0` (2026-11-02) | `Aggregate { handle(cmd) → events via EventSink; apply(event) }` (`cqrs@37c1ab0:src/aggregate.rs:118-167`). `EventStore { load_events, load_aggregate, commit }` (`src/store.rs:9-35`). **Stateless per command**: `execute` does load → handle → commit every time (`src/cqrs.rs:173-183`). Upcasters (`src/persist/upcaster.rs:8-12`) | `postgres-es` 0.5.0, `mysql-es` 0.5.0 (sqlx); `sqlite-es` 0.5.0 (third-party, sqlx) | Don't depend: no resident actor, and `String` metadata. **Copy** the table: `PRIMARY KEY (aggregate_type, aggregate_id, sequence)` (`persistence/postgres-es/db/init.sql:2-12`), and SQLSTATE `23505` → `OptimisticLock` (`persistence/postgres-es/src/error.rs:17-24`). |
| `eventually` | 0.4.0 (2020-10-04) on crates.io | repo `get-eventually/eventually-rs@8a842d7` (2025-11-21, dependency bumps); `main` is an **unpublished 0.5.0** whose `eventually-postgres` uses sqlx 0.8 (`eventually-postgres/Cargo.toml:4,23`) | `Streamer::stream(id, VersionSelect)` and `Appender::append(id, version::Check, events) → Version`; `Check::{Any, MustBe(v)}` (`eventually/src/event/store.rs:16-61`, `eventually/src/version.rs:16-23`) | yes, on `main` only | Don't depend (no release since 2020). **Copy the trait split**: a read side and an append side with an explicit expected-version check. It is the closest match to #49. |
| `disintegrate` | 4.0.0 (2026-02-02) | active; `disintegrate-es/disintegrate@33f12a0` (2026-08-03) | Query-based consistency instead of aggregates (README:12-19). `append(events, query, last_event_id)` re-runs the query and fails on newer matching events (`disintegrate/src/event_store.rs:46-71`) | `disintegrate-postgres` 4.0.1 (sqlx) | Don't depend. Its cross-stream consistency solves a problem a single-writer instance does not have. Reconsider for flows that decide across instances. |
| `esrs` | 0.18.0 (2024-11-25) | repo active (`primait/event_sourcing.rs@079e579`, 2026-10-27), no release in 2025–26 | Pure `handle_command(&State, Command) → Vec<Event>` and `apply_event(State, Event) → State` (`src/aggregate.rs:41-47`). `EventStore::lock` takes a **Postgres advisory lock** per aggregate (`src/store/postgres/event_store.rs:176-181`). Unique `(aggregate_id, sequence_number)` index (`src/sql/postgres/migrations/`) | built in (sqlx feature) | Don't depend: "Prima.it-opinionated", `Uuid` ids. **Note** the advisory lock as a cheap cross-process single-writer option. |
| `eventastic` | 0.5.0 (2025-09-23) | quiet; `jdon/eventastic@8f7596d` (2025-09-23) | Fork of eventually: mandatory transactions, idempotency by event id, optimistic concurrency, transactional outbox, snapshots (`README.md:5-13`). `PRIMARY KEY (aggregate_id, version)` plus `UNIQUE (event_id)` (`eventastic_postgres/migrations/20230610185630_init.sql:7-10`) | `eventastic_postgres` (sqlx) | Don't depend (single maintainer). **Copy** the idempotency-by-event-id unique index and the outbox idea for effects. |

No crate found by name for a generic "sqlx event store". Each sqlx-backed store above is tied to its own framework's types [S24].

**Verdict [inference]:** depend on none. Every crate assumes either load-per-command (cqrs-es, esrs, eventastic) or its own actor runtime (coerce). keelrs's needs are small: one stream per instance, one writer, an append with expected seq, read-after, and a live tail. Copy four patterns:
1. `eventually`'s Streamer/Appender split with `Check::MustBe`.
2. `cqrs-es`'s primary key and `23505` mapping.
3. `eventastic`'s event-id idempotency.
4. `coerce`'s actor hooks.

---

## 5. keelrs's actor today, in Akka terms

All references are at `fpaschos/keelrs@29b20b5`.

- **Handle and actor.**
  - `AgentInstance` is a cloneable handle around `mpsc::Sender<Command>` (`keelrs/src/agent/instance.rs:34-38`).
  - `spawn` creates the inbox (capacity 1024, `:28`), a child cancellation token of the runtime's shutdown token, an empty `settled` map, and `tokio::spawn(actor.run())` (`:42-57`).
  - `Runtime::instance_with` keeps one handle per `(agent name, InstanceId)` in a `parking_lot::Mutex<HashMap>` and spawns on first use (`keelrs/src/runtime.rs:29, 99-111`).
  - Dropping the `Runtime` cancels every instance (`runtime.rs:116-120`).
- **Commands.** `Command::Dispatch { submission_id, text, structured }` and `Command::Read { submission_id, respond_to }` (`instance.rs:153-164`). `dispatch` replies with a `Receipt` once the command is *enqueued* (`:114-125`), not once it is stored.
- **Effect.** `InstanceActor::dispatch` runs the whole Rig run inline (`rig::run(...)`, `:222-228`), raced against cancellation (`:229-240`). Model and tool calls happen during the command, with no record before them.
- **Events.** `Event::Message(Message)` and `Event::Settled { submission_id, result }` (`keelrs/src/event.rs:15-23`). They are built (`instance.rs:235, 241-244`) and then dropped. `// STORE: append events to durable storage, before answering reads` (`:245`).
- **State.**
  - `settled: HashMap<SubmissionId, Result<SettledValue, Outcome>>`, marked "STORE: stands in for the stored `Event::Settled`s" (`:185-186`). `read` answers from it (`:251-260`).
  - History is not state at all: `let history = Vec::new()` (`:219-220`), so each dispatch starts blank.
  - `caches` is explicitly "never stored" (`:187-188`).
- **Single writer.** It holds in one process: one task per key, and commands are serial (`:193-210`). A `Read` waits behind any running dispatch, because the inbox is FIFO (`:177-179`).
- **Identity.** `SubmissionId` is a process-global `AtomicU64` (`:267-275`), so receipts do not survive a restart.

**Mapping.** Command is `Dispatch`/`Read`. The events are `Event::Message` and `Event::Settled`. State is meant to be a fold of those events, but today it is an in-memory `settled` map and an empty history. The effect is the Rig run. Reply is the `Receipt` at enqueue and the `SettledValue` on read.

**Missing**, against Akka:
- **Persist.** No journal and no `seq`.
- **Recover.** No replay on spawn and no `RecoveryCompleted` step that resolves open intents.
- **Snapshot.** None.
- **Stash.** Nothing to stash during yet, since recovery does not exist.
- **Passivation.** Actors and the `settled` map live until the runtime drops, and the map grows without bound.
- **Re-attach by id.** It works within a process; across a restart it returns a blank instance.
- **Single writer across processes.** Two processes, or one restarted process with a zombie task, could both write.
- **Query/subscribe.** There is no read-from-seq and no live tail.

---

## 6. Adapting keelrs's actor (inference)

All of this section is **[inference]**: the smallest shape that makes `InstanceActor` an event-sourced actor. It lists names and responsibilities, not an implementation. The design questions it touches stay open in their tickets.

### 6.1 The event store: a trait plus a sqlx backend (#49, #46)

The actor talks to one keelrs-owned trait. It follows the `eventually` split, `Orleans CustomStorage`'s expected version and `cqrs-es`'s table.

```rust
/// One stream per agent instance (later: per flow). Store-assigned `seq` is dense from 1.
#[async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Atomically appends `events` at `expected + 1 ..`; fails with `Conflict { actual }`
    /// if the stream's last seq is not `expected` (or the writer epoch is stale).
    async fn append(&self, stream: &StreamId, expected: Seq, writer: Epoch,
                    events: Vec<NewEvent>) -> Result<Seq, AppendError>;
    /// Finite read of stored events with `seq > after`, in order (Akka `currentEventsByPersistenceId`).
    fn read(&self, stream: &StreamId, after: Seq) -> BoxStream<'_, Result<StoredEvent>>;
    /// Claims the stream for a new incarnation: bumps and returns its writer epoch (6.7).
    async fn acquire(&self, stream: &StreamId) -> Result<Epoch>;
    // Later: load_snapshot / save_snapshot(through_seq, reducer_version, state).
}
```

- `StoredEvent` is #46's envelope: `stream`, `seq`, `event_id`, `type`, `schema_version`, `payload`, `recorded_at`, causation and correlation ids, optional `idempotency_key`. The store assigns `seq` and `recorded_at`.
- **sqlx backend.**
  - One `keelrs_events` table with `PRIMARY KEY (stream, seq)`, so a duplicate `seq` is SQLSTATE `23505`, mapped to `Conflict` as in `cqrs-es`.
  - A `UNIQUE (stream, idempotency_key)` index, as `eventastic` has on `event_id`.
  - A `keelrs_streams (stream, agent, data, last_seq, writer_epoch)` row holds instance data. Per #49, stored data wins on restart.
  - `append` runs in one transaction that checks `writer_epoch` and inserts the rows.
  - Postgres and SQLite are both sqlx targets. Which ones ship is #49's call.
- An **in-memory store** implements the same trait for tests and apps without a database (#49 asks).
- The store does **not** need a native subscribe in a single process (6.8).

### 6.2 Command handling: append with expected `seq`, then reply (#48)

- The actor keeps `seq: Seq` (last appended) and `epoch: Epoch`. Each command handler decides events, calls `store.append(stream, self.seq, self.epoch, events)`, folds them into state, then replies or performs effects. This is Akka's persist → event handler → `thenRun`/`thenReply`, with "reply after durable" as the default.
- `Dispatch` appends a `Submitted { submission_id, text, structured_schema }` event **before** returning the `Receipt`. A receipt then means "durably accepted", and it survives a restart. That needs a durable `SubmissionId`, such as a UUID v7 or the event's `(stream, seq)`, instead of the process `AtomicU64`.
- Inside the run, append per step (#48), following the earlier note's intent-before-effect rules: `ModelRequested` → call → `ModelResponded`; `ToolCallStarted` → call → `ToolCallCompleted`. Side effects only start after their intent is stored.
- A `Conflict` on append means another writer exists. The actor **stops** (fail-stop) and fails its pending replies as aborted. It never retries blindly, because its state is stale.

### 6.3 State as a fold of events (#47, #48)

```rust
struct InstanceState { history: Vec<Message>, submissions: HashMap<SubmissionId, Submission>, /* open intents */ }
impl InstanceState { fn apply(&mut self, event: &StoredEvent); } // pure: no clocks, no I/O
```

- `apply` replaces both `settled` and the empty `history`. The same function runs during recovery and after each append, as with Akka's event handler and Orleans `TransitionState`.
- `caches` stays outside the fold, as it is today ("never stored").

### 6.4 Recovery on spawn

`InstanceActor::run` gains a recovery phase before the inbox loop:
1. `acquire` the writer epoch.
2. Load the snapshot, if any, then `read(stream, after)` and fold.
3. Set `seq`.
4. Run a `recovery_completed` step. It resolves open intents by #49's resume rules: re-issue model calls; settle each unfinished tool call as interrupted unless the tool is declared safe to re-run. It also re-attaches the sandbox (by Sandbox key, per #49). This is Akka's `RecoveryCompleted`, the one place where at-least-once effects re-run.

### 6.5 Stash during recovery

- No new type is needed. The mpsc inbox already buffers commands until recovery finishes, so it is the stash, bounded by `INBOX_CAPACITY`.
- One difference from Akka is deliberate. Akka drops commands when the stash is full [S1]. A bounded tokio channel makes senders wait.
- The stash covers persists too, because the actor awaits each append before taking the next command, as with Akka's automatic stash during persist.
- `Read` for a pending submission should not hold the inbox (#48). Park its `respond_to` in the state's `Submission` and answer it when `Settled` is applied. That requires the run to move off the actor's own `await`: a spawned run task sends step results back as inbox messages, Akka's "pipe to self". This is the main structural change, and the pick between it and today's FIFO waiting belongs to #48.

### 6.6 Passivation and re-attach by id

- **Passivate.** An idle timeout (`tokio::time::sleep` in the `select!`, reset on each command) stops an actor with no run in flight and no parked reads. Before it exits, it closes its receiver and drains what is left.
- **Re-attach.** For passivation to be invisible, `AgentInstance` should address the instance by key, not by a raw `mpsc::Sender`. On a closed inbox, the handle asks the runtime registry to re-activate the key: spawn, which recovers from the store. This matches Akka's `EntityRef` and Orleans' grain reference.
  - The registry entry needs an "activating" state, so that concurrent callers wait for one incarnation instead of spawning two. **[inference]** This replaces the unconditional `or_insert_with` (`runtime.rs:104`).
- **After a process restart.** `Runtime::instance(agent, id)` simply recovers from the store, so re-attach by id needs nothing more once 6.4 exists.

### 6.7 One process first; then fencing; then sharding (#49)

1. **One process.** The registry guarantees one task per key, and expected-`seq` on append catches bugs and zombie tasks.
2. **Several processes on one database (fencing).** `acquire` bumps `writer_epoch` on the stream row, and `append` checks `writer_epoch = $epoch` in the same transaction. A stale incarnation's next append fails, and it stops. This gives the Durable Streams producer epoch from the earlier note without a coordinator.
   - Alternative: `esrs`'s per-stream Postgres advisory lock (`event_sourcing.rs@079e579:src/store/postgres/event_store.rs:176-181`). It blocks instead of failing, needs a held connection per active instance, and is Postgres-only.
3. **Routing (sharding).** With fencing, two processes can never both write, but they can both *try*. Routing each instance id to one owner process (consistent hashing, a lease table, or the host platform, as Durable Objects does) stops the ping-pong. That belongs outside keelrs's core, behind the same `Runtime::instance` call.

### 6.8 The read/subscribe path (#49)

- `AgentInstance::events(after: Seq) -> impl Stream<Item = StoredEvent>` does catch-up then live, the earlier note's rule 13. It reads `store.read(stream, after)` until caught up, then follows a `tokio::sync::broadcast` the actor publishes to **after** each successful append. That is Akka's "effects after persist", and the output gate of Durable Objects.
- On `RecvError::Lagged`, re-read from the store. Every item carries `seq`, so gaps are detectable.
- A passivated instance has no broadcast. A subscriber either re-activates it or polls the store. Across processes, a Postgres `LISTEN/NOTIFY` per append is the natural live signal. Which to ship is open.
- `read(receipt)` answers from folded state, or parks the request (6.5). After a restart, it is answered from the stored `Settled` event.

### 6.9 Not yet

- **Snapshots.** Conversations replay quickly at first. Add `(through_seq, reducer_version, state)` rows when replay time shows up. A reducer-version bump re-folds from 0 (rule 5 of the earlier note).
- **Schema evolution.** `schema_version` sits on the envelope from day one (#46). Upcast in `apply`'s deserialize step, Akka-style [S4].
- **Projections across instances.** Akka `eventsByTag`-style reads need a global position column. Defer until a flow needs it.

---

## Open questions

- Akka's docs give no exact wording on what the journal does when two writers race. The "different messages with the same sequence number" failure comes from the replay-filter section, so whether a given journal plugin rejects duplicates depends on that plugin [S1].
- The Restate docs fetched do not say how state writes and journal entries are committed relative to each other beyond "recorded alongside execution steps" [S21].
- Dapr's docs do not state the default actor idle timeout on the pages read [S22, S23].
- The Rust crate maintenance status is a reading of release dates and last commits on 2026-11-04, not maintainer statements.

## Sources

- [S1] Akka, Event Sourcing (typed persistence): https://doc.akka.io/libraries/akka-core/current/typed/persistence.html
- [S2] Akka, Snapshotting: https://doc.akka.io/libraries/akka-core/current/typed/persistence-snapshot.html
- [S3] Akka, Cluster Sharding: https://doc.akka.io/libraries/akka-core/current/typed/cluster-sharding.html
- [S4] Akka, Schema Evolution for Event Sourced Actors: https://doc.akka.io/libraries/akka-core/current/persistence-schema-evolution.html
- [S5] Akka, Persistence Query: https://doc.akka.io/libraries/akka-core/current/persistence-query.html
- [S6] Apache Pekko 1.7.1, Event Sourcing: https://pekko.apache.org/docs/pekko/current/typed/persistence.html
- [S7] Orleans overview: https://learn.microsoft.com/en-us/dotnet/orleans/overview
- [S8] Orleans grain lifecycle (GrainCollectionOptions): https://learn.microsoft.com/en-us/dotnet/orleans/grains/grain-lifecycle
- [S9] Orleans grain directory implementation: https://learn.microsoft.com/en-us/dotnet/orleans/implementation/grain-directory
- [S10] Orleans request scheduling: https://learn.microsoft.com/en-us/dotnet/orleans/grains/request-scheduling
- [S11] Orleans grain persistence: https://learn.microsoft.com/en-us/dotnet/orleans/grains/grain-persistence/
- [S12] Orleans JournaledGrain API: https://learn.microsoft.com/en-us/dotnet/orleans/grains/event-sourcing/journaledgrain-basics
- [S13] Orleans log-consistency providers: https://learn.microsoft.com/en-us/dotnet/orleans/grains/event-sourcing/log-consistency-providers
- [S14] Cloudflare, What are Durable Objects: https://developers.cloudflare.com/durable-objects/concepts/what-are-durable-objects/
- [S15] Cloudflare, SQLite-backed Durable Object Storage API: https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/
- [S16] Cloudflare blog, "Durable Objects: Easy, Fast, Correct — Choose three": https://blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three/
- [S17] Cloudflare, Durable Object State API (`blockConcurrencyWhile`): https://developers.cloudflare.com/durable-objects/api/state/
- [S18] Cloudflare, Durable Object lifecycle: https://developers.cloudflare.com/durable-objects/concepts/durable-object-lifecycle/
- [S19] Restate, Services (Virtual Objects): https://docs.restate.dev/foundations/services
- [S20] Restate, TS state: https://docs.restate.dev/develop/ts/state
- [S21] Restate, key concepts: https://docs.restate.dev/foundations/key-concepts
- [S22] Dapr, Actors overview: https://docs.dapr.io/developing-applications/building-blocks/actors/actors-overview/
- [S23] Dapr, Actor runtime features: https://docs.dapr.io/developing-applications/building-blocks/actors/actors-features-concepts/
- [S24] crates.io API (`/api/v1/crates/<name>` and `/dependencies`), read 2026-11-04, for ractor, ractor_cluster, kameo, kameo-persistence, actix, coerce, cqrs-es, postgres-es, mysql-es, sqlite-es, eventually, eventually-postgres, disintegrate, disintegrate-postgres, esrs, eventastic
- [S25] Source, read 2026-11-04: `slawlor/ractor@1af0aaa`, `tqwewe/kameo@1b6bffb`, `actix/actix@8e04016`, `LeonHartley/Coerce-rs@a2d3f19`, `serverlesstechnology/cqrs@37c1ab0`, `get-eventually/eventually-rs@8a842d7`, `disintegrate-es/disintegrate@33f12a0`, `primait/event_sourcing.rs@079e579`, `jdon/eventastic@8f7596d`
- [S26] keelrs source: `fpaschos/keelrs@29b20b5` — `keelrs/src/agent/instance.rs`, `keelrs/src/runtime.rs`, `keelrs/src/event.rs`
- [S27] keelrs tickets: "The event envelope" https://github.com/fpaschos/keelrs/issues/46 ; "Who writes events, and when" https://github.com/fpaschos/keelrs/issues/48 ; "Event store and resume" https://github.com/fpaschos/keelrs/issues/49
- [S28] Earlier note: `docs/research/event-sourced-harnesses.md` on branch `research/event-sourced-harnesses`
- [S29] Earlier notes: `docs/research/flue-render-and-messaging.md`, `docs/research/archon-agent-runtime.md`, `docs/research/rig-harness-sessions.md` on their `research/*` branches
- [S30] Alice Ryhl, "Actors with Tokio": https://ryhl.io/blog/actors-with-tokio/
