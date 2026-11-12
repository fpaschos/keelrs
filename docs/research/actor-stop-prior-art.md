# Prior art: what a single-writer actor does when its lease or store fails

Question: keelrs's instance actor resets on a store failure and lives on. The proposal is an Akka-style STOP lifecycle: one incarnation acquires the lease and recovers once; on any lease or store failure it releases the lease, fails its waiting callers and stops; the next `Runtime::instance(..)` starts a new incarnation. What do other systems do?

Researched 2026-11-12. Primary sources were read on that date. Flue is read from a local clone at `deee93b4` (2026-10-12). Orleans source is `dotnet/orleans@cc476682fd` (main). **[inference]** marks a conclusion that no source states.

## keelrs today

- `reset` drops the run, the lease, the fold and the caches, and fails parked reads. "the next command recovers afresh" (`keelrs/src/agent/instance/actor.rs:388-402`).
- `reset` sets `self.lease = None` and does not call `store.release` (`actor.rs:391-393`). **[inference]** After a store failure, the lease stays held until its TTL expires; only a later `acquire` by this same actor or another can take it after expiry.
- Decided on #48: "Any store failure (`Fenced`, `Failed`, a lost commit reply): drop the running step, `Folded` and the epoch; everyone waiting gets the error; the next command acquires and recovers afresh. The actor never dies, so handles and the runtime's map stay valid." (#48 resolution comment, "Inside the actor").
- The earlier repo research recommended the opposite: "A `Conflict` on append means another writer exists. The actor **stops** (fail-stop) and fails its pending replies as aborted. It never retries blindly, because its state is stale." (`docs/research/durable-actors.md` §6.2, branch `research/durable-actors`). The same document proposes re-attach on a closed inbox: "On a closed inbox, the handle asks the runtime registry to re-activate the key ... This matches Akka's `EntityRef` and Orleans' grain reference", with an "activating" registry state so concurrent callers do not spawn two incarnations (§6.6).

## Summary table

| System | On failure | Who restarts | In-memory state reused? | References valid? | Source |
|---|---|---|---|---|---|
| Akka/Pekko classic persistence | Persist **failure**: actor "will unconditionally be stopped". Persist **rejection** (e.g. serialization): "the actor continues with next message". Recovery failure: "the actor will be stopped". | A `BackoffSupervisor` the user adds: "better to stop the actor and after a back-off timeout start it again". | No. A new incarnation replays the journal. | Restart keeps the `ActorRef`; stop does not. With sharding, callers hold an `EntityRef`, so yes. | [A1], [P1] |
| Akka Typed `EventSourcedBehavior` | "By default an EventSourcedBehavior will stop if an exception is thrown from the journal." `PersistFailed` / `RecoveryFailed` signals; `EventRejectedException` goes to the supervisor (default: stop). | Opt-in: `onPersistFailure(SupervisorStrategy.restartWithBackoff(..))`. "it isn't valid to resume a behavior on a journal failure as it is not known if the event was persisted." | No. Restart reinstalls the original behavior and recovers again. | Same `ActorRef` across a supervised restart; stashed commands are kept on a persist-failure backoff, dropped on a thrown exception. | [A2], [A3], [A4] |
| Akka/Pekko Cluster Sharding | An entity that stops is gone; messages that arrive during passivation are buffered and "delivered to a new incarnation". Shard lease: "If the lease is lost after initialization the Shard will be terminated." | The shard, on the next message ("Without remembered entities restarts happen on the arrival of a message"); with remember-entities, after `entity-restart-backoff` (10 s). Without the lease, a shard "will remain uninitialized" and its messages are buffered in the `ShardRegion`. | No. A new incarnation recovers from the journal. | Yes: `EntityRef` names the entity id, not the incarnation. | [A5] |
| Akka coordination `Lease` | `acquire(leaseLostCallback)`; `checkLease` "should return false if the lease is lost". The owner must act ("shutdown shards or singletons"); the API does not act for it. | The user of the lease (sharding: shard terminated, re-created on a later message). | n/a | n/a | [A6] |
| Temporal | A failed Workflow Task: "the Worker removes that Workflow Execution from its cache". A worker that does not take its sticky task in time loses stickiness; the task goes back to the normal queue. | The Temporal Service reschedules; any worker picks up the next Workflow Task. | No. "An evicted Workflow Execution will need to be replayed"; after a crash "the Worker uses the Event History to replay the code". | Yes: callers address the Workflow Id; the cache is a worker-side optimization. | [T1], [T2], [T3] |
| Orleans (grain persistence, journaling) | An `InconsistentStateException` (ETag conflict) that escapes a grain call deactivates that activation. Journaling docs: "The manager fences operations and requests deactivation; a fresh activation recovers durable state." A read failure at activation fails the activation and the caller's request. | The runtime, on the next call to the grain (virtual actor). | No. A fresh activation reads storage. | Yes: grain references are logical ids. | [O1], [O2], [O3] |
| Marten / Wolverine | Append with a stale expected version: "Marten will reject the changes and throw its `ConcurrencyException`". No resident actor. | Wolverine's generic error-handling policy on the message (retry re-runs the handler, which loads the aggregate again). **[inference]**: the page names no retry default. | No. Each handler run fetches the stream. | n/a (no actor) | [M1] |
| Flue | A failed append moves the `ConversationRecordWriter` to `failed`: it rejects pending flushes, unpins its fold and calls `onFailed`, which removes it from the coordinator's map. The next use creates a new writer, which calls `acquireProducer` again. A lost submission claim throws "attempt lost ownership"; a replaced attempt's session "goes inert". | The coordinator: on next use (new writer) and by reconcile (a new `attemptId` through a fencing CAS, then resume from the stream). Heartbeat renew failure is only logged and deferred. | Partly. The writer and attempt are new; the shared fold host may hand back a fold it already holds for the same stream incarnation. | Callers address agent + instance id, not a writer. | [F1]–[F5] |

## Notes per system

### Akka / Pekko Persistence

- Classic: "If persistence of an event fails, `onPersistFailure` will be invoked (logging the error by default), and the actor will unconditionally be stopped." Rejection: "`onPersistRejected` will be invoked (logging a warning by default), and the actor continues with next message." Recovery: "`onRecoveryFailure` is called (logging the error by default), and the actor will be stopped." "It is better to stop the actor and after a back-off timeout start it again" with `BackoffSupervisor` [A1] (Pekko identical [P1]).
- The split matters for keelrs: a **rejection** is known not stored (the actor's state is still right), so it continues; a **failure** is unknown, so it stops. keelrs's `StoreError::Fenced` and a lost commit reply are both "unknown or stale" cases. **[inference]** A pre-append check failure that keelrs finds before calling the backend (e.g. a numbering error) is the rejection case.
- Typed: stop by default on a journal exception; override "with any BackoffSupervisorStrategy" via `onPersistFailure`; "Stashed commands are preserved and processed later in case of a failure while storing events" (backoff case), but "Stashed commands are discarded in case the actor is restarted (or stopped) due to a thrown exception" [A2]. Typed default supervision is stop ("Typed actors are stopped if an exception is thrown and no supervision strategy is defined") [A3]. A supervised restart re-installs the original behavior; the mailbox stays and "the restart is not visible outside of the actor itself" [A3], [A4].

### Akka / Pekko Cluster Sharding and lease

- Entities start on message arrival; passivation buffers: "Incoming messages will be buffered by the Shard between reception of Passivate and termination of the entity ... delivered to a new incarnation of the entity." Idle passivation default "timeout of 2 minutes". Remember-entities restarts "after a rebalance or entity crash" after `entity-restart-backoff` (default 10 s) [A5].
- Lease: "each shard won't create child entity actors unless it has the lease"; "If a shard can't acquire a lease it will remain uninitialized" and messages are "buffered in the ShardRegion"; "If the lease is lost after initialization the Shard will be terminated"; acquisition retries every `lease-retry-interval` (5 s) [A5].
- **[inference]** The docs do not say what happens to messages sent to an entity that stops itself without `Passivate` (e.g. after a persist failure). Messages buffered during passivation go to the new incarnation; this is the closest documented case.
- Lease API: loss is a callback plus a synchronous `checkLease`; the heartbeat timeout "should be greater than the max expected JVM pause", and "there will be a short time before the original lease owner can take action e.g. shutdown shards or singletons" [A6]. This matches keelrs's local deadline guard (#48: "refuses to start tools once it has passed").

### Temporal

- "Workers cache the state of the Workflow they execute." "If a Workflow Task fails, the Worker removes that Workflow Execution from its cache"; if the sticky worker does not start the task in time, "the Temporal Service disables stickiness ... reschedules the Workflow Task in the original queue" [T1].
- "An evicted Workflow Execution will need to be replayed when it gets any action that may advance it" [T2]. "if the Worker crashes, the Worker uses the Event History to replay the code ... It then resumes progress from the point of failure" [T3].
- Pattern: in-memory state is a cache; any doubt discards it; the next task rebuilds it from history. The addressable id (Workflow Id) never changes.

### Orleans

- Runtime source: "If a grain allowed an inconsistent state exception to escape and the exception originated from this activation, then deactivate it." (`src/Orleans.Runtime/Core/InsideRuntimeClient.cs:368-379`, `DeactivationReasonCode.ApplicationError`) [O1].
- Journaling operations guide: on `InconsistentStateException` during a write, and on a write whose acknowledgement is lost, "The manager fences operations and requests deactivation; a fresh activation recovers durable state" / "the new activation observes the committed mutation" [O2]. This is exactly the "lost commit reply" case in #48.
- Read failure at activation: it "fail[s] the activation operation for that grain ... The original request to the grain that caused the activation faults back to the caller" [O3]. Grains are virtual, so the next call activates again [O3]; the earlier repo research quotes "Actors are purely logical entities that *always* exist, virtually" (`docs/research/durable-actors.md`, Orleans section).

### Marten / Wolverine

- Marten checks the expected version and throws `ConcurrencyException` [M1]. It has no resident single-writer actor, so there is no lease to lose; Wolverine's aggregate handler loads the stream per message. Repo research cites Marten only for the store shape (#48: "Typed envelope columns + the body as one opaque JSON column is what Flue, Archon, iterate, Akka/Pekko, Marten and Temporal do").

### Flue

- Writer lifecycle `active | failed`; `fail()` sets `failed`, `foldHost.unpin()`, calls `onFailed`, clears pending records and rejects the pending flush (`packages/runtime/src/conversation-writer.ts:299-317`) [F1]. Every later call throws the stored error (`assertActive`, `:299-301`).
- The Node coordinator's `onFailed` removes the writer from its map: `if (conversationWriters.get(path) === writer) conversationWriters.delete(path);`; the next `getConversationWriter` creates a new one, which calls `store.acquireProducer` again (`packages/runtime/src/node/agent-coordinator.ts` `getConversationWriter`; `conversation-writer.ts` `create`) [F2]. **[inference]** This is a STOP lifecycle: the failed writer is never reused, and a new incarnation starts on the next use.
- Submissions: "a zombie session that lost its claim can neither steal deliveries nor corrupt their state" (`agent-submissions.ts:97-102`); on input application, a lost claim throws "Agent submission attempt lost ownership before input application." (`:860-865`); an interrupted submission gets a "replacement attempt (the fencing CAS)" and resumes from the stream (`:624-689`) [F3].
- Lease heartbeat: renew every 10 s; a failed renew is logged and emitted as `deferred`, with no stop (`agent-coordinator.ts:523-541`) [F4]. Hung attempts are aborted at their deadline by `enforceActiveDeadlines` (`:594-610`).
- Fold reuse: the shared fold host "reuses a fold a read already paid for", keyed by stream incarnation (`conversation-writer.ts` `loadReducedState`; `conversation-fold-host.ts:30,65-103`) [F5]. **[inference]** Flue reuses a fold only when it is known to equal the stream head, not after a failure in doubt.

## What this means for the proposal (inference)

- Every system with a resident in-memory actor (Akka, Orleans, Temporal workers, Flue writers) **discards** that incarnation on a store failure in doubt and rebuilds from the log. None keeps the same incarnation alive and resets it in place. keelrs's `reset` is equivalent in effect (state dropped, recovery on the next command) but keeps the task.
- Default is **stop, then start on the next message**, with backoff only as an opt-in (Akka `BackoffSupervisor`, `onPersistFailure`) or for remember-entities. Orleans and sharded Akka start the new incarnation on the next call.
- Held references stay valid in every system because callers hold a **logical id** (`EntityRef`, grain reference, Workflow Id, agent + instance), not the incarnation. For keelrs, STOP keeps `AgentInstance` valid only if the handle re-resolves through the runtime registry on a closed inbox (`durable-actors.md` §6.6), or the registry hands out a new handle and old handles fail.
- Messages in flight to a stopping incarnation: Akka buffers them in the shard and delivers them to the new incarnation (passivation case); Akka drops stashed commands when the actor stops on an exception; Orleans faults the triggering call. keelrs must choose between failing queued commands with the error and forwarding them to the next incarnation.
- Lease release on stop: Akka sharding terminates the shard on lease loss; Flue's failed writer does not release a claim explicitly. keelrs's `reset` today does not call `release`; the proposal's "release the lease" is stricter than Flue and frees the instance before TTL.
- Akka separates **rejection** (known not stored: continue) from **failure** (unknown: stop). keelrs could keep this split: stop on `Fenced`, `Failed` and a lost reply; continue on an error that is known to leave the stream unchanged.

## Sources

- [A1] Akka classic Persistence, Failures: https://doc.akka.io/libraries/akka-core/current/persistence.html
- [P1] Apache Pekko classic Persistence, Failures: https://pekko.apache.org/docs/pekko/current/persistence.html
- [A2] Akka Typed Event Sourcing (journal failures, stash): https://doc.akka.io/libraries/akka-core/current/typed/persistence.html
- [A3] Akka Typed fault tolerance: https://doc.akka.io/libraries/akka-core/current/typed/fault-tolerance.html
- [A4] Akka supervision and monitoring: https://doc.akka.io/libraries/akka-core/current/general/supervision.html
- [A5] Akka Typed Cluster Sharding (passivation, remember entities, Lease): https://doc.akka.io/libraries/akka-core/current/typed/cluster-sharding.html
- [A6] Akka Coordination (Lease API): https://doc.akka.io/libraries/akka-core/current/coordination.html
- [T1] Temporal, Sticky Execution: https://docs.temporal.io/sticky-execution
- [T2] Temporal, Workflow cache tuning: https://docs.temporal.io/develop/worker-performance/workflow-cache
- [T3] Temporal, Event History: https://docs.temporal.io/encyclopedia/event-history
- [O1] Orleans source, `InsideRuntimeClient.cs`: https://github.com/dotnet/orleans/blob/cc476682fd/src/Orleans.Runtime/Core/InsideRuntimeClient.cs#L368-L379
- [O2] Orleans journaling operations guide: https://github.com/dotnet/orleans/blob/cc476682fd/docs/site/src/content/docs/grains/journaling/operations.md (table "Diagnose common failures")
- [O3] Orleans grain persistence, Failure modes: https://learn.microsoft.com/en-us/dotnet/orleans/grains/grain-persistence/
- [M1] Wolverine, Aggregate handlers and event sourcing with Marten: https://wolverinefx.net/guide/durability/marten/event-sourcing.html
- [F1]–[F5] Flue, local clone `/Users/fpaschos/Dev/Work/sapp/flue` at `deee93b4`: `packages/runtime/src/conversation-writer.ts`, `packages/runtime/src/node/agent-coordinator.ts`, `packages/runtime/src/runtime/agent-submissions.ts`, `packages/runtime/src/conversation-fold-host.ts`.
- Repo: #48 resolution comment; #49 "From The durable actor"; `docs/research/durable-actors.md` and `docs/research/event-sourced-harnesses.md` on branches `research/durable-actors` and `research/event-sourced-harnesses`.
