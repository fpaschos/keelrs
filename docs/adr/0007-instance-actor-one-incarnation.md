# An agent instance actor stops on a failure; the library restarts it in place behind its ActorRef

An agent instance's actor (`InstanceActor`) is built by each **start**: it takes its lease and recovers its stream, serves, and **stops** on any lease or store failure (`Held` or `InstanceDataConflict` at its start, `Fenced`, `Failed`, a failed renewal, the deadline guard). The library owns that loop and knows no lease: the actor holds its lease, renews it on a scheduled command (`Context::schedule`), and on a stop releases it and stashes what it still owes (the failing call, the parked reads); the library then fails the stash and the inbox with the cause. Each key's task is spawned once and keeps its mailbox, so the next command **restarts** the actor in place (Akka's supervision restart: new state, same `ActorRef`, same mailbox). An `AgentInstance` holds that `ActorRef` for good. When no actor answers (the runtime was dropped), the call fails with `Error::InstanceUnavailable`. This reverses [#48](https://github.com/fpaschos/keelrs/issues/48)'s "the actor never dies", decided on [The keelrs actor pattern](https://github.com/fpaschos/keelrs/issues/98).

## Considered Options

- **`reset`** (#48): the task stays alive after a store failure, drops its fold and re-acquires on the next command. It forgot the lease without releasing it, so the next command got `Held` until the TTL ran out, and a second recovery inside one state is state that must be reset field by field.
- **Re-attach by key on the caller side** (Akka `EntityRef` style, the first build in PR #107): each restart had its own channels, so every send locked a per-key slot to find the live one, and a closed inbox meant spawning a new task and resending. It cost a lock on every send and leaked spawning into the caller.
- **Each actor writes its own loop** (Ryhl, the second build in PR #107): the instance repeated the library's lifecycle by hand (wait for the first command, release before answering, drain, carry reads over a stop). An `Actor` trait with hooks keeps that in one place.
- **Visible stale handles** (Akka's plain stop): the caller gets an error and asks `Runtime::instance` again. Every application that retries repeats that wiring.

## Consequences

- Only the failing call, and the calls waiting with it, see the cause; the next command restarts the actor. Nothing queued is carried over a failure.
- Commands sent while the actor starts wait in its inbox, in order: the start is the stash.
- `Runtime::instance` takes no lease; the key's task waits for its first command. `Runtime::start` checks for a Tokio runtime once (`NoTokioRuntime`) and keeps its handle for every spawn.
- The library is the crate-private `actors` module: a generic core (`ActorRef`, the `Actor` trait with `start`, `handle` and `stopped`, a `Context` with `pipe_to_self`, `unpipe`, `stash`, `hold` and `guard`, and `spawn`), keyed `Entities`, and a persistent `Lease` the context renews and guards. The actor value is its state (`type Args` builds it; no separate state type). The library knows no actor's commands; the agent instance is its first actor, and pipes each step to itself. Timers wait for their first user (passivation).
- `abort()` is a graceful stop followed by an instance command that settles the queued dispatches as `Aborted`. A graceful stop keeps the inbox, and the actor stashes its parked reads for the next start. Passivation will stop an idle actor the same way, behind the same `ActorRef`.
