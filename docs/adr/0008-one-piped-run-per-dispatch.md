# One piped run per dispatch persists through its actor

A dispatch's run is **one future**, `Run::drive`, piped to the instance actor once ([ADR 0007](0007-instance-actor-one-incarnation.md)). It drives Rig's `AgentRun` to its end ([ADR 0004](0004-keelrs-drives-rigs-agentrun.md)) and stores what it produced before it goes on, write-ahead as [#48](https://github.com/fpaschos/keelrs/issues/48) decided: each model answer (B) and each tool result (C) goes through a `PersistSink`, and the run returns `RunCompleted` (its last events, its result and the caches) as D. The actor implements the sink with `Writer`, which asks the actor to store (`Command::Persist`) and waits for the answer; one persist at a time is a compile-time rule (`&mut self`). The run knows no actor and no store, so a test drives it with a `Vec` as its sink. Decided on [The run](https://github.com/fpaschos/keelrs/issues/108).

The cadence, the stored events, their order and their dispatch ids are those of the stepper this replaces.

## Considered Options

- **Keep the stepper** (`Run::step`, PR #101): the actor piped each step to itself and got the `Run` back in `Command::Stepped`. The run's loop state lived in fields (`next`, `calls`, `pinned`) so it could stop between steps, and the actor re-piped after each append. One loop in one future reads top to bottom and needs none of that state.
- **One append per turn**: store a model answer together with its tool results. A tool's side effect would then happen before anything about it is stored, against #48's write-ahead rule.
- **One batched append per batch**: store a batch's tool results once the whole batch has run. One slow tool would hold back every finished result, and a crash would lose them all.

## Consequences

- `persist` has no error: it returns once the events are stored, and when they cannot be stored it never returns. The actor stops on the failure, and the stop drops the run.
- Abort drops the actor's `PipeGuard`, which cancels the run. A `Persist` the run had already sent is skipped: its dispatch is no longer the running one.
- A failed run returns its reason; the actor adds the closing events (an "aborted" result for each unanswered tool call) when it settles the dispatch, as before.
- How keelrs drives `AgentRun` by hand (the `produced` cursor against Rig's batch commit, `result_of`'s copy of the full history, `call_model`'s many jobs) is left as it was, for [Driving Rig's AgentRun](https://github.com/fpaschos/keelrs/issues/111).
