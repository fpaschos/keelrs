# An agent instance actor lives one incarnation; handles re-attach by key

An agent instance's actor takes its lease and recovers its stream once, serves, and **ends** on any lease or store failure (`Held` or `InstanceDataConflict` at its start, `Fenced`, `Failed`, a failed renewal, the deadline guard). Its end is plain code, in this order: release the lease, close the inbox, answer every waiting caller with the cause. An `AgentInstance` names the instance by key (Akka's `EntityRef`, an Orleans grain reference), not by its task: the next call after an end spawns a new incarnation, which recovers from the store. When no actor answers (the runtime was dropped, or the actor ended without answering), the call fails with `Error::InstanceUnavailable`. This reverses [#48](https://github.com/fpaschos/keelrs/issues/48)'s "the actor never dies", decided on [The keelrs actor pattern](https://github.com/fpaschos/keelrs/issues/98).

## Considered Options

- **`reset`** (#48): the task stays alive after a store failure, drops its fold and re-acquires on the next command. It forgot the lease without releasing it, so the next command got `Held` until the TTL ran out, and a second recovery inside one task is state that must be reset field by field.
- **Visible stale handles** (Akka's plain stop): the caller gets an error and asks `Runtime::instance` again. Every application that retries repeats that wiring; the transparent reference keeps it in keelrs.

## Consequences

- Only the failing call, and the calls waiting with it, see the cause; the call after re-attaches. A command the inbox never took is resent once to the new incarnation; a command the actor took is never resent (`InstanceUnavailable`).
- Commands sent while an actor starts wait in its inbox, in order: the start is the stash.
- `Runtime::instance` spawns nothing and takes no lease; the first call does. `Runtime::start` checks for a Tokio runtime once (`NoTokioRuntime`) and keeps its handle for every later spawn.
- The common parts live in the crate-private `actors` module: the base (`Mailbox`), the keyed layer (`Entities`, `EntityRef`) and the single writer (`Lease`). Passivation will end an idle actor the same way.
