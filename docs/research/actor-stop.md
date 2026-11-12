# STOP lifecycle for the agent instance actor

Question: what changes in keelrs if the instance actor stops on any lease or store failure (one incarnation: acquire and recover once, then end), instead of the `reset` that #48 decided?

Parts, each with its sources:

- [actor-stop-tickets.md](actor-stop-tickets.md): the map (#43), every affected ticket, ADRs, glossary, PR #101, the grilling questions.
- [actor-stop-code.md](actor-stop-code.md): code impact at `8d4a1c5`, tests, examples and boilerplate, abort.
- [actor-stop-prior-art.md](actor-stop-prior-art.md): Akka/Pekko, Temporal, Orleans, Marten/Wolverine, Flue.

## Summary

- Every system checked drops its in-memory state after a store failure and rebuilds it from the log. None resets in place. Akka Persistence stops the actor; Orleans deactivates the grain; Temporal evicts and replays; Flue's writer removes itself and a new one claims again.
- Callers keep valid references in all of them, because they hold a logical id (`EntityRef`, grain reference), not the incarnation. The earlier keelrs research said the same: fail-stop (`durable-actors.md` §6.2) and handles that re-attach by key through the runtime (§6.6). The #48 Resolution chose `reset` instead, and planned "private respawning handles" for passivation later.
- So STOP has two forms:
  - **visible**: a held handle returns a new error, and the caller calls `Runtime::instance` again;
  - **private**: the handle re-attaches through the runtime, and the caller sees only the original error once.
  The private form keeps #48's passivation plan and the public API; the visible form adds an `Error` variant.
- Code: `reset`, the re-acquire path in `hold` and the `Occupied` branch of `Runtime::instance` change; the store, fold, stepper, `RuntimeBuilder` and `abort` stay. About +20 to +40 lines net. `Error` is not `Clone`, so failing many callers with one cause needs a copy strategy.
- Ordering: close the inbox and release the lease before answering the failing command, or a re-fetch gets the dying handle, or the new actor gets `Held` for up to the TTL.
- Examples: 0 added lines; all use one in-memory runtime, where the stop path is almost unreachable.
- Tests: no test reaches today's `reset`. New tests need a failing `StoreBackend` through the public `Store::new`.
- Tickets that change: #48, #97, #49, #92, #98, #102, #43, #72. A new ADR is recommended. The glossary lists "stop" under **Abort**'s _Avoid_, so the actor's end needs another word.
- Today's code has two wrong answers whatever is decided: a handle to a gone actor returns `Aborted` for a dispatch never stored, and `abort()` returns `Ok`.
