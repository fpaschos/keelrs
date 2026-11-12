# STOP: a single-incarnation instance actor — code research

All citations are `path:line` at commit `8d4a1c5`. "Inferred" marks a conclusion not stated in the code.

**STOP** (the proposal): the actor acquires the lease and recovers once, on its first command. On any lease or store
error (Fenced, Failed, failed renew, local deadline guard) it releases the lease best-effort, fails parked reads and the
commands still in its inbox with the real cause, and ends. It never re-acquires. `Runtime::instance(..)` spawns a fresh
actor when the stored handle's inbox is closed. Held handles get a new error (e.g. `Error::InstanceStopped`) after the end.

**Today**: a store failure calls `reset` (`keelrs/src/agent/instance/actor.rs:389-402`), which drops the run, the fold,
the lease and the caches and fails parked reads; the actor stays alive, and the next command re-acquires and re-recovers
through `hold` (`actor.rs:183-198`).

## A. Ergonomics

### Public API that matters

- `Runtime::instance` / `instance_with` (`keelrs/src/runtime.rs:62-135`): the `Occupied` branch returns a clone of the
  stored handle after the data check (`runtime.rs:115-121`); it never looks at whether the actor is alive.
- `AgentInstance` (`keelrs/src/agent/instance/mod.rs:33-153`): `id`, `dispatch`, `ask`, `ask_structured`, `read`,
  `abort`. It is `Clone` (`mod.rs:32`), so a user can keep it as long as they like.
- `ask_structured` hides the receipt (`mod.rs:87-97`): on a failed read the caller has no `dispatch_id` to read again,
  so the only "retry" open to a user is a new dispatch (inferred from the signature).
- Store errors reach users as `Error::Store(StoreError::{Held, Fenced, Failed})` (`keelrs/src/error.rs:55-57`,
  `keelrs/src/store/mod.rs:129-153`). `Error` is `#[non_exhaustive]` (`error.rs:9`), so a new variant is not a breaking
  change for exhaustive matches.

### Examples

| Example | Handle kept across calls? | Across failures? | Store | Boilerplate added by STOP |
|---|---|---|---|---|
| `hello-world` | no: one `ask` (`examples/hello-world/src/main.rs:23-24`) | no: `?` ends `main` | in-memory (`Runtime::start`, `:21`) | 0 |
| `instructions` | no: one `ask` (`examples/instructions/src/main.rs:25-26`) | no: `?` | in-memory (`:23`) | 0 |
| `tools` | no: temporary handle (`examples/tools/src/main.rs:41-44`) | no: `?` | in-memory (`:39`) | 0 |
| `skills` | no: three temporary handles, three agents (`examples/skills/src/main.rs:69-90`) | no: `?` | in-memory (`:62`) | 0 |
| `build-review-loop` | **yes**: `implementing` and `reviewing` live across up to `MAX_ROUNDS = 3` rounds (`examples/build-review-loop/src/main.rs:151-189`, `:31`); `planned` used once (`:134-139`) | no: every call ends in `?` (`:166`, `:170-172`) | in-memory (`:131`) | 0 |

No example uses a `Store` other than the default in-memory one (no `.store(` in `examples/*/src`), and no example
matches on an error or retries. Every failure ends `main` with `?`. So STOP adds **0 boilerplate lines** to every
example, and the "Example readability" metric (AGENTS.md "Success metrics") is unchanged.

Can STOP's error even fire in the examples? With the in-memory store and one runtime:
- `renew` fails only for a stale or released epoch (`keelrs/src/store/memory.rs:53-69`); the actor's own epoch never
  goes stale without a second runtime (inferred).
- `append` fails only on a wrong epoch or head (`memory.rs:100-122`); the actor is the only writer (inferred).
- The local deadline guard (`actor.rs:264-268`) trips only if the actor task is not polled for about 0.9 TTL − TTL/3 ≈
  17 s of the 30 s default (`store/mod.rs:35`, `actor.rs:369`, `actor.rs:406-408`): renewal is a `select!` arm
  alongside the running step (`actor.rs:96-106`), so only a blocking (non-async) tool or model client could starve it
  (inferred).

So in today's examples the stop path is practically unreachable; the behaviour change matters for SQL stores and two
runtimes.

### What a resilient user program needs (not in any example today)

Today (handle survives; next command re-acquires):

```rust
let implementation = loop {
    match implementing.ask_structured::<Implementation>(&ask).await {
        Err(Error::Store(_)) => continue, // same handle; the actor re-recovers
        other => break other?,
    }
};
```

Under STOP (handle is dead; re-fetch it):

```rust
let implementation = loop {
    match implementing.ask_structured::<Implementation>(&ask).await {
        Err(Error::Store(_) | Error::InstanceStopped) => {
            implementing = runtime.instance_with(&implementer, id.clone(), issue.clone())?; // + `let mut`, keep `id`
            continue;
        }
        other => break other?,
    }
};
```

Delta: about +2 lines per retry site (the re-fetch, `let mut`, and keeping `id`/`data` in scope), plus one more error
variant to match. In `build-review-loop` that would be 2 sites (`:166`, `:170-172`), ~+4 lines — but only if the
example chose to retry, which it does not. Both versions also share a hazard: a retry re-dispatches, because
`ask_structured` loses the receipt (`mod.rs:87-97`); the recovered actor settles the started dispatch as `Aborted`
(`actor.rs:220-227`) and queued ones still run (inferred).

## B. Code impact at 8d4a1c5

### How `submit` / `settled` / `abort` behave on a closed channel today

- `submit` (`mod.rs:125-140`): `self.inbox.send(command)` errors on a closed channel; the error is ignored (`:135`). The
  `SendError` owns the command, so `respond_to` is dropped, `response.await` fails, and the caller gets
  `Error::Run { outcome: Aborted, dispatch_id }` (`:136-138`, `aborted` at `:155-160`). That `dispatch_id` is fresh
  (`:126`) and was never stored, so the "aborted" receipt names a dispatch that does not exist (inferred).
- `settled` (`mod.rs:143-152`): same path, `Error::Run { Aborted }` (`:149-151`).
- `abort` (`mod.rs:117-122`): send error ignored, `response.await.unwrap_or(Ok(()))` → `Ok(())`.
- Commands still queued when the actor ends are dropped with the receiver at the end of `run` (`actor.rs:94-114`), so
  they too become `Run { Aborted }` (inferred from `mod.rs:134`).
- Today the actor ends only on runtime drop (`actor.rs:99`) or when every sender is gone (`actor.rs:107-110`); the
  runtime's map holds a sender for its whole life (`runtime.rs:35`), so in practice only runtime drop ends it (inferred).

### What goes (or shrinks)

| Code | Where | ~Lines |
|---|---|---|
| `reset`: drops run, lease, fold, caches; fails parked reads | `actor.rs:389-402` | −14 (its parked-read failure moves into the stop path, net ~−6) |
| `Folded::default()` re-fold after reset | `actor.rs:394` | −1 |
| `caches = Caches::default()` after reset | `actor.rs:395` | −1 |
| `hold`'s "already holding" guard and the re-acquire it enables | `actor.rs:185-188` | −4 if acquire+recover moves to a one-time start (see below) |
| `hold()` calls in each handler | `actor.rs:139-142` (read), `:173` (admit), `:306` (abort) | −6 if recovery runs once before the loop |
| `lease: Option<(Epoch, Instant)>` and its unwraps | field `actor.rs:49-51`, init `:83`, `release` `:118`, `append` `:327` (`expect`), `renew` `:357`, `renewal` `:367` (`?`), `trusts_lease` `:378-379`, `OptionFuture` for renewal `:96` | ~−6, if the lease becomes non-`Option` after a one-time start (inferred: possible only if the loop starts after the first command's acquire) |
| `Err(self.reset(...))` call sites | `actor.rs:268`, `:334`, `:358-360` | rewritten, not removed |
| Doc lines that describe recovering afresh | `actor.rs:61-63`, `:324-325`, `:389-390`; `mod.rs:4-8`, `:64-68`; `runtime.rs:21-24`, `:58-59` | rewritten |

Note: `Folded` itself (`fold.rs:26-72`) is unchanged: STOP still folds once on recovery (`actor.rs:202-212`) and after
each append (`actor.rs:337-350`).

Two shapes for "recover once" (a design decision, AGENTS.md "Code" → State/Concurrency):
1. Keep `hold` and `Option` lease; `hold` is reached only before the first acquire. Smallest diff: ~−20 lines (reset
   and its call sites), Option handling stays.
2. `run` waits for the first inbox/control message, acquires and recovers, then enters the loop with a plain
   `(Epoch, Instant)`. Removes `hold`, the three `hold()` calls and the `Option` handling: ~−35 lines, +~10 for the
   pre-loop step. The first message must then be handled after recovery, which is what `hold` does today (inferred).

### What is added

| Addition | Where | ~Lines |
|---|---|---|
| Loop exit from handlers: `command`, `stepped`, `abort`, `renew` return a stop signal (e.g. `Result<(), Error>` or `ControlFlow`) and `run` breaks on it | `actor.rs:94-114`, `:123`, `:258`, `:304`, `:354` | +10–15 |
| Stop path: `inbox.close()` (so `Sender::is_closed` turns true at once), release, fail `waiting`, drain `inbox` (`try_recv`) and `control`, answering each with the cause | new fn near `actor.rs:389` | +20–25 |
| Release on stop | reuse `release` (`actor.rs:116-121`), already called after the loop (`:113`) | 0 |
| Cloneable cause for many receivers | `StoreError` is `Clone` (`store/mod.rs:130`); `Error` is not (`error.rs:8`), and `InstanceDataConflict` (from `recover`, `actor.rs:213-218`) is an `Error`, so the drain needs a clone strategy (inferred) | +3–5 |
| `AgentInstance::is_closed()` (`pub(crate)`) | `mod.rs` near `:58` | +4 |
| Respawn on closed inbox | `runtime.rs:115-121`: after the data check, if `instance.is_closed()`, spawn and replace (as `:122-132`) | +8 (or restructure both arms, ~+4) |
| `Error::InstanceStopped` (or similar) with doc | `error.rs:10-58` | +5–8 |
| `submit` / `settled` map a closed inbox to the new error instead of `aborted` | `mod.rs:134-138`, `:149-151` | ~+4, and `aborted` (`:155-160`) may go if no caller remains |
| `abort` on a dead actor: keep `Ok(())` or return the new error | `mod.rs:119-121` | 0–2 |
| Docs: actor lifetime, handle validity, re-fetch rule | `actor.rs:1-2`, `:93`; `mod.rs:29-31`, `:64-68`, `:113-116`; `runtime.rs:19-24`, `:58-76` | +8–12 |

Rough net: shape 1 ≈ +40 lines, shape 2 ≈ +20 lines in `keelrs/src` (inferred estimates). Public surface: +1 `Error`
variant; nothing else public.

### Ordering hazards (inferred)

- **Close before answering.** If the actor answers the failing command before `inbox.close()`, a caller that re-fetches
  at once can get the same dying handle back from `runtime.rs:115-121`.
- **Release before answering.** The in-memory store keeps the lease until `release` (`memory.rs:38-51`, `:71-78`). If the
  caller re-fetches before the old actor releases, the fresh actor's `acquire` gets `Held` until the TTL runs out (30 s,
  `store/mod.rs:35`). After `Fenced`, release with the stale epoch is a no-op (`memory.rs:71-77`; store contract
  `a_released_lease_is_free_at_once_and_a_stale_release_is_a_no_op`, `store/testing.rs:70`), which is correct.
- **`Held` on the first acquire.** STOP's list (Fenced, Failed, renew, deadline) does not name `Held`. Today `Held`
  returns through `?` at `actor.rs:190` and leaves the actor untouched. If `Held` also ends the actor, every
  "wait for the lease" loop must re-fetch the handle (see C). Open decision.
- **Runtime drop.** `actor.rs:99` breaks on cancel; under STOP the drain should answer queued commands with
  `InstanceStopped` rather than letting them become `Run { Aborted }` (`mod.rs:134`).

## C. Tests

### `keelrs/tests/durable.rs`

| Test | Line | Under STOP |
|---|---|---|
| `dispatch_once_free` (helper) | `:33-43` | Retries on `Held` with the **same** handle. If `Held` ends the actor, it must take the runtime, agent, id and data and re-fetch each try. Unchanged if `Held` does not stop. |
| `a_later_runtime_recovers_the_instance_and_continues_its_stream` | `:46-64` | Passes as is if `Held` does not stop. Otherwise via the changed helper; the handle is also reused for two more calls after the helper (`:61`, `:63`), so the helper must return the live handle. |
| `a_second_runtime_is_held_off_until_the_first_drops` | `:67-87` | `other` gets `Held` (`:78-82`) and is reused at `:85-86`. If `Held` stops, it needs a re-fetch from `second`. |
| `other_data_for_a_stored_instance_conflicts_at_dispatch` | `:90-107` | Passes: the first error is the conflict from `recover` (`actor.rs:213-218`); after it the actor ends (if recovery failure stops), not checked by the test. |
| `abort_with_nothing_running_does_nothing` | `:110-120` | Passes if abort keeps the actor alive. If abort stops it, the dispatch at `:117` gets `InstanceStopped`: test must re-fetch, and its name/claim changes. |

New tests the change needs (all reachable through public seams):
- **A held handle after a stop gets `InstanceStopped`, and `Runtime::instance` returns a live one that recovers the
  stream.** Trigger: a custom `StoreBackend` via the public `Store::new` (`store/mod.rs:162`, `:225`) whose `renew` or
  `append` returns `Fenced`/`Failed` on demand; or two runtimes with a short `lease_ttl` (`store/mod.rs:241`) where the
  first is starved — the backend route is deterministic (inferred).
- **Queued commands are failed with the real cause, not `Run { Aborted }`.** Same trigger, several `read`s parked.
- **Released on stop:** after the stop, a second runtime acquires at once (no `Held`).

### `keelrs/src/agent/instance/tests.rs`

| Test | Line | Under STOP |
|---|---|---|
| `a_second_dispatch_runs_after_the_first_ones_messages` | `:106-135` | Unchanged (fold + `Run` only). |
| `an_aborted_dispatchs_open_tool_calls_are_answered_before_the_next_one` | `:137-176` | Unchanged (fold + `closing` only). |
| `a_read_that_recovers_the_stream_runs_its_queued_dispatches` | `:221-238` | Unchanged: a read as the first command recovers once. |
| `abort_settles_dispatches_it_has_not_recovered_yet` | `:240-260` | Unchanged if abort keeps the actor alive (abort is the first message and recovers, `actor.rs:305-306`). If abort stops it, the reads at `:247-258` get `InstanceStopped`; the test must re-spawn with `spawned` (`:209-219`) to read the `Aborted` results from the stream. |

### `keelrs/tests/dispatch.rs`, `turn_setup.rs`

No test there keeps a handle across a store failure; all use the in-memory store with one runtime
(e.g. `an_existing_id_with_equal_data_continues_the_same_instance`, `dispatch.rs:112-128`). Unchanged (inferred).

### Store contract (`keelrs/src/store/testing.rs`)

Unchanged: STOP does not touch `StoreBackend` (`store/mod.rs:162-200`). The tests at `testing.rs:52-150` keep their
meaning.

## D. Abort: stop the actor, or keep it alive

Today `abort` recovers if needed (`actor.rs:305-306`), settles the running and every queued dispatch as `Aborted` in one
append (`:307-319`), and the actor keeps serving (`mod.rs:113-116`: "With nothing running or queued, does nothing").

**Abort keeps the actor alive** (no change):
- Docs and both abort tests stand (`durable.rs:110-120`, `tests.rs:240-260`).
- A store error during abort's append (`actor.rs:319`) is a stop like any other.
- Abort as the first message still acquires the lease and recovers, which is what lets it see queued dispatches
  (`actor.rs:305`).

**Abort stops the actor**:
- Every abort forces a re-fetch: the user's next `dispatch` on the same handle gets `InstanceStopped`. `Runtime`
  respawns, the new actor re-acquires and re-reads the whole stream (`actor.rs:202-206` reads from `Seq::default()`), so
  each abort costs a full recovery (inferred).
- Abort on an idle instance (today a no-op, `actor.rs:307-309`) would still end it, or the rule becomes "stops only if
  something was pending" — two behaviours to document (inferred).
- Gain: abort also releases the lease at once, so another runtime can take the instance without waiting (inferred).
  No example or test needs that today.
- `abort` on an already-dead actor returns `Ok(())` today (`mod.rs:120-121`). Under STOP, with either choice, a dead
  actor's queued dispatches are still stored and **will run** when a fresh actor recovers (`actor.rs:220-227` settles
  only a started one; `start_next` at `:196` runs the rest). So `Ok(())` from a dead actor is a silent false positive;
  it should return `InstanceStopped` (inferred).
- Both abort tests and the `abort` docs (`mod.rs:113-116`) change.

Recommendation (inferred): keep the actor alive after abort; return `InstanceStopped` from `abort` on a dead actor.
