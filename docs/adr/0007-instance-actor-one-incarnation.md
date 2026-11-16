# An agent instance actor stops on a failure; the library starts it again in place behind its ActorRef

An agent instance's actor (`InstanceActor`) lives one **incarnation** at a time, with Akka's default stop. Each **start** takes its lease and recovers its stream; it then serves, and **stops** on any lease or store failure (`Fenced`, `Failed`, a failed renewal, the deadline guard). The library owns that loop and knows no lease. On a stop it drops what was piped and scheduled and the inbox (their callers get `Error::InstanceUnavailable`), then calls `post_stop(self, cause)`: the actor releases its lease and answers its parked reads with the cause. Each key's task is spawned once and keeps its mailbox, so the next command **starts** the actor again in place (cluster sharding's `EntityRef`: new state, same `ActorRef`, same mailbox). An `AgentInstance` holds that `ActorRef` for good. This reverses [#48](https://github.com/fpaschos/keelrs/issues/48)'s "the actor never dies", decided on [The keelrs actor pattern](https://github.com/fpaschos/keelrs/issues/98); the lifecycle below was settled on [The run](https://github.com/fpaschos/keelrs/issues/108).

The start is **lazy**: nothing runs until the first command, after the spawn and after each stop. When `start` fails (`Held`, `InstanceDataConflict` against the stored data), the hook `start_failed(first, cause)` answers the command that triggered it with the cause, because `Held` is how a replica learns that another one owns the instance (sharding by lease); the queued commands are dropped. Akka has no such hook: it starts eagerly.

keelrs has **three layers**, each knowing only the one below it:

1. **`actors`**, a mini library inside keelrs: `ActorRef`, the `Actor` trait (`start`, `handle`, `post_stop`, `start_failed`), `Context` (`self_ref`, `pipe_to_self`, `schedule`), `spawn`/`spawn_with`, and keyed `Entities`. It knows no agent, store, lease or event, and grows only when a core actor needs a feature.
2. **`InstanceActor`**, keelrs core: it owns the store, the lease, the fold and the queue, and it is the single writer.
3. **the Rig run**, keelrs core: a plain future that knows no actor and no store ([ADR 0008](0008-one-piped-run-per-dispatch.md)).

## Considered Options

- **`reset`** (#48): the task stays alive after a store failure, drops its fold and re-acquires on the next command. It forgot the lease without releasing it, so the next command got `Held` until the TTL ran out, and a second recovery inside one state is state that must be reset field by field.
- **Re-attach by key on the caller side** (the first build in PR #107): each restart had its own channels, so every send locked a per-key slot to find the live one. It cost a lock on every send and leaked spawning into the caller.
- **Each actor writes its own loop** (Ryhl, the second build in PR #107): the instance repeated the library's lifecycle by hand. An `Actor` trait with hooks keeps it in one place.
- **A stash that carries owed answers over a stop** (the third build in PR #107): `stash`, a `Command::fail` trait method and a graceful/failed `Stop` let the library fail or keep each queued command. Akka's way is simpler: the actor answers what it owes in `post_stop`; the rest is dropped.
- **Visible stale handles** (Akka's plain stop): the caller gets an error and asks `Runtime::instance` again. Every application that retries repeats that wiring.

## Consequences

- A failure drops the failing command and the inbox: their callers get `InstanceUnavailable`, and parked reads get the cause. Nothing queued is carried over a stop. The inbox is dropped before the hooks answer anyone, so a caller that retries once answered reaches the next start.
- Commands sent while the actor starts wait in its inbox, in order.
- `Runtime::instance` takes no lease; the key's task waits for its first command. `Entities::get(key, args)` takes eager, owned `Args` (`Arc<InstanceArgs>`), so the instance data check moved to `dispatch`: the `AgentInstance` handle keeps its data, and a `Dispatch` with other data than the instance's fails with `InstanceDataConflict`.
- `pipe_to_self` returns a `#[must_use]` `PipeGuard`: dropping it cancels its future, `detach` lets it run until a stop. The instance holds its run as `running: Option<PipeGuard>`.
- `abort()` is one instance command in the same incarnation: it drops `running` (cancelling the run) and settles every dispatch that is not settled as `Aborted`. It does not stop the actor, keeps the lease, and does not recover again. Parked reads get `Aborted` when it is stored.
- `ActorRef::stop` and the system lane stay for passivation: a requested stop is `post_stop(None)`, and the next command starts the actor again.
- The `Actor` hooks are `async fn` through `#[trait_variant::make(Send)]`, a bridge until return type notation is stable (rust-lang/goals#646).
