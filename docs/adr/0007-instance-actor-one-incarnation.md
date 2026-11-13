# An agent instance actor lives one incarnation; its ActorRef restarts it in place

An agent instance's actor takes its lease and recovers its stream once per **incarnation**, serves, and ends that incarnation on any lease or store failure (`Held` or `InstanceDataConflict` at its start, `Fenced`, `Failed`, a failed renewal, the deadline guard). The end is plain code: release the lease, then answer the failing call, the parked reads and the queued commands with the cause. Each key's task is spawned once and keeps its mailbox, so the next command starts a new incarnation **in place** (Akka's supervision restart: new state, same `ActorRef`, same mailbox). An `AgentInstance` holds that `ActorRef` for good. When no actor answers (the runtime was dropped), the call fails with `Error::InstanceUnavailable`. This reverses [#48](https://github.com/fpaschos/keelrs/issues/48)'s "the actor never dies", decided on [The keelrs actor pattern](https://github.com/fpaschos/keelrs/issues/98).

## Considered Options

- **`reset`** (#48): the task stays alive after a store failure, drops its fold and re-acquires on the next command. It forgot the lease without releasing it, so the next command got `Held` until the TTL ran out, and a second recovery inside one state is state that must be reset field by field.
- **Re-attach by key on the caller side** (Akka `EntityRef` style, the first build in PR #107): each incarnation had its own channels, so every send locked a per-key slot to find the live one, and a closed inbox meant spawning a new task and resending. It cost a lock on every send and leaked spawning into the caller.
- **Visible stale handles** (Akka's plain stop): the caller gets an error and asks `Runtime::instance` again. Every application that retries repeats that wiring.

## Consequences

- Only the failing call, and the calls waiting with it, see the cause; the next command is served by a new incarnation. Nothing queued is carried over.
- Commands sent while an incarnation starts wait in its inbox, in order: the start is the stash.
- `Runtime::instance` takes no lease; the key's task waits for its first command. `Runtime::start` checks for a Tokio runtime once (`NoTokioRuntime`) and keeps its handle for every spawn.
- The library is the crate-private `actors` module: a generic core (`ActorRef`, `Mailbox` with a system lane for `Stop` and `pipe_to_self` for work the actor runs in its own task, `spawn`), keyed `Entities`, and a persistent `Lease` the mailbox renews and guards. It knows no actor's commands; the agent instance is its first actor, and pipes each step to itself. Timers wait for their first user (passivation).
- `abort()` is a system `Stop` followed by an instance command that settles the queued dispatches as `Aborted`; its public meaning does not change. Passivation will end an idle incarnation the same way, behind the same `ActorRef`.
