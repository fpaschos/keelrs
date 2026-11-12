# Actor STOP lifecycle: map, tickets, ADRs, glossary, PR #101

Scope: items 1, 2, 3 and 7 of the research brief. Ergonomics, code impact and other projects are in separate notes.
Sources: GitHub issues on `fpaschos/keelrs` (read 2026-11-12), PR #101, and the tree at `8d4a1c5`.
"Inferred" marks a conclusion that no source states.

## Question

The proposed change: the agent instance actor gets an Akka-style STOP lifecycle with one incarnation per actor.
It acquires the lease and recovers once, on its first command. On any lease or store error it releases the lease
best-effort, fails parked reads and queued commands with the real cause, and ends. `Runtime::instance(..)` spawns
a fresh actor when the stored handle's inbox is closed. Old handles get a new error (e.g. `Error::InstanceStopped`).
Open: does `abort()` also stop the actor?

Which tickets, ADRs and glossary terms does this change, and what happens to PR #101?

## Summary

1. The change reverses one bullet of the #48 Resolution: "The actor never dies, so handles and the runtime's map stay valid."
2. It also contradicts #48's passivation note: "It can be added later without public change: an `idle_timeout` setter, and private respawning handles." STOP with `InstanceStopped` is a public change; private respawning handles are not.
3. On the map (#43), it belongs between #97 and #102. #102 rewrites the instance tests through `Runtime`; STOP must land first, or those tests are written twice (inferred).
4. Tickets that change: #48 (one bullet, one fog note), #97 (behaviour, tests, open items), #49 ("Store failures already reset the actor"), #92 (who owns the per-instance broadcast), #98 (shutdown), #102 (tests), #43 (map text). #47, #91, #94 and #99 stay, with small notes.
5. No ADR records the actor lifecycle today. A new ADR is recommended, because STOP reverses a recorded decision and adds a public error (inferred).
6. Glossary: **Abort** lists "stop" under _Avoid_, and the map's fog uses "`uid` incarnation" for another idea. A term for the actor's end must avoid both.
7. STOP fixes #97's second open item (a reset forgets the lease without releasing it).
8. PR #101: amend it, do not close it. The store contract, fold, stepper, `RuntimeBuilder` and `abort()` do not depend on the failure lifecycle. Run a short grilling first, because this is a Concurrency and Contracts decision (AGENTS.md).
9. The biggest open decision: caller-visible `InstanceStopped`, or handles that respawn privately (as #48 planned for passivation).
10. Whether `abort()` stops the actor depends on decision 9. With caller-visible stops, abort-then-dispatch on one handle breaks.

## Map position

The map is #43, "Durable, observable agent instances: actor, store, events" (`wayfinder:map`, open). Its **Order**:

> 1. Identity (#46) → Implement identity (#94)
> 2. The durable actor (#48) → Implement the durable actor (#97), then Implement a scripted model (#102) (so the actor pattern and Restart are tested through `Runtime`), then The keelrs actor pattern (#98) (grill → task)
> 3. Storage (#91)
> 4. The agent instance's events (#47)
> 5. Restart (#49) (moved ahead of the bus on The durable actor)
> 6. Event bus (#92)
> 7. Write throughput (#99) (after Storage and the bus; measured)
> 8. Tracing via the bus (#93)
> 9. OTel and GenAI spans (#36)

- The durable actor is step 2. #48 and #94 are closed. #97 is open, built in PR #101 (open, not a draft, head `8d4a1c5`).
- Next on the map: #102 (`wayfinder:task`), then #98 (`wayfinder:grilling`).
- The map's rule: "after a grilling resolves, its implementation `wayfinder:task` is worked before the next grilling" (#43, Priority rules).
- Fog ("Not yet specified", #43) that STOP touches:
  - "Passivation: idle actors stop and release their lease, handles respawn them (a private change, plus an `idle_timeout` on `RuntimeBuilder`); needed when memory or idle lease renewal (~1 write/s per 10 live instances) grows."
  - "The `uid` incarnation, once instances can be deleted or reset."
- #43's "Decisions so far" summarises #48 without the lifecycle bullet; it needs one line if STOP is adopted.
- #72 (flow run, open) also asks about the instance lifecycle: "The runtime's instance map only grows … Can an idle one be evicted? Evicting needs [#49] to reload it."

Proposed position (inferred): a new grilling ticket, "The instance actor's lifecycle: stop on lease loss", between #97 and #102:
`#48 → #97 (amended) → new grilling → #102 → #98`. If #101 is amended, the new ticket's task is #97 itself.

## Ticket-by-ticket impact

| Ticket | Decision today | Change under STOP | Stays |
|---|---|---|---|
| #43 map (open) | Order step 2: "#48 → #97, then #102 … then #98". Fog: passivation is "a private change … handles respawn them". | Add the lifecycle grilling to step 2 before #102. Rewrite the passivation fog line: the stop path exists; only the idle trigger is left. | Every other order step; out-of-scope list. |
| #46 Identity (closed) | Row 5: "No attempt or run id. Restarts are recorded as ordinary events (`Recovered` …)". Fog: "`uid` incarnation and writer epoch stay in the map's fog." | None to the decisions. The word "incarnation" is taken by the fog's `uid` idea; do not reuse it for an actor's life (inferred). | Stream key, `DispatchId`, `seq`, no attempt id. |
| #47 Events (open) | Event payloads only; "created", "dispatched", "message", "tool_result", "settled" built provisionally by #97. | None stored. An actor that lost the lease cannot append a "stopped" event (the epoch check refuses it, #48: "`append` … only while the epoch and the head still match"). A live-only "stopped" event belongs to #92 (inferred). | All. |
| #48 Durable actor (closed) | Resolution, Inside the actor: "**Any store failure** (`Fenced`, `Failed`, a lost commit reply): drop the running step, `Folded` and the epoch; everyone waiting gets the error; the next command acquires and recovers afresh. The actor never dies, so handles and the runtime's map stay valid." Not now: "Passivation … can be added later without public change: an `idle_timeout` setter, and private respawning handles." Recovery: "acquire, read from 0, `apply` each event, then serve the inbox (it is the stash)." | That bullet is reversed: release best-effort, fail waiters and queued commands, end the task; the runtime spawns a fresh actor. The passivation note's "without public change" no longer holds if handles see `InstanceStopped`. Record the reversal on the new grilling ticket (#48 is closed). | Lease with epoch; store contract; write-ahead cadence A–D; stepper; fold; recovery on first command with the inbox as stash; loop priority; local deadline guard (its action changes from reset to stop); abort on the control channel; restart policy "continue" handed to #49. |
| #49 Restart (open) | "**Store failures** already reset the actor (drop state, re-acquire, recover on the next command); a lease taken over by another replica means that replica's recovery handles the dispatch." Q3: "at start, the runtime spawns the instances that have unsettled dispatches." | Rewrite that bullet: a store failure stops the actor; the next `runtime.instance(..)` spawns a fresh one that recovers. Q3 gains weight: after a stop with no takeover, an unsettled dispatch waits until someone calls `instance(..)` again, or the runtime sweep runs (inferred; today it waits for the next command on the same handle, `actor.rs:185-198`). | Policy "continue"; the recovery seam "after the fold and before the inbox is served" (`actor.rs:220-227`); idempotency keys; sandbox and instance data questions. |
| #72 Flow run (open) | "The runtime's instance map only grows … Can an idle one be evicted?" | STOP adds dead entries to that map (closed handles) until `instance(..)` replaces them. Eviction of closed entries is a sub-question (inferred). | Its other questions. |
| #91 Storage (open) | sqlx backend; "Retry on lock conflict: a transaction that hits SQLite busy … is rolled back and rerun"; Q8 released versus expired. | None to the contract or SQL. Note: a transient `Failed` from `renew` now ends the actor; in-backend retry on busy (already planned) keeps that rare (inferred). STOP releases on failure, so Q8's "renew after own release is `Fenced`" check matters more. | Tables, lease SQL, contract suite, connect call. |
| #92 Event bus (open) | Q2: "a `tokio::sync::broadcast` per instance stream, owned by the actor." Q5: "the actor's own happenings (started, recovering, fenced, a store failure; passivation is not built …) are ephemeral bus events". | An actor-owned broadcast ends with the actor, so every subscriber's stream ends on a stop. Decide: the runtime owns the broadcast, or subscribers resubscribe from their last `seq` (Q3 already supports this). Add "stopped" to Q5's list. | Publish after commit; catch-up then tail on `seq`; slow subscribers. |
| #94 Implement identity (closed) | `instance_with` keeps "each instance's data as `serde_json::Value` next to its `AgentInstance`"; equal data continues, other data → `InstanceDataConflict`. | None. A respawn reuses the kept data (`runtime.rs:35`, `:115-121`). A new `Error` variant follows #94's shape (`{ agent, id }`). | All. |
| #97 Implement the durable actor (open; PR #101) | Behaviour: "**Recovery** (`hold`, on the first command, or after any store failure)"; "**Deadline guard:** … the actor resets as `Fenced`"; "**Any store failure:** drop the run, the fold and the lease; fail parked reads; stay alive." Tests: "`Held` until the first runtime drops". Open: "A reset forgets the lease without releasing it, so the actor's next command gets `Held` until the store's lease expires". | Rewrite those three Behaviour bullets; drop "or after any store failure". Add: runtime respawn, the new error, queued commands failed with the cause. The second Open item is fixed by STOP's best-effort release. The `Held` test retries `dispatch` on the same handle (`tests/durable.rs:33-43`, `:67-86`); if `Held` stops the actor, it must re-fetch the handle. Metrics change. | Public store contract, `Store`, `StoredEvent`, `StoreError`, `Epoch`, `Seq`, contract tests, `RuntimeBuilder`, `abort()` signature, `Run::step`, `Folded`, write-ahead admission, `Runtime` drop releasing leases. |
| #98 Actor pattern (open) | "Decide what is common: handle and ask-with-reply, control priority and abort, shutdown, and how per-actor sources … plug into the loop." Spawning: "The pattern's spawn function owns [the Tokio check]: it does the check and returns `Result`". | STOP decides what "shutdown" means for keelrs actors: end on failure, owner respawns. The pattern should carry it (inferred). The runtime's respawn is a second `spawn` call site, so the Tokio check moving into `spawn` matters twice. | No actor crate; extract from working code. |
| #99 Write throughput (open) | "Idle lease renewal traffic, and whether passivation (map fog) is the answer." "Nothing here may change the actor, the `StoreBackend` contract or the write-ahead rule." | With STOP, passivation is only an idle trigger on the existing stop path, which makes it cheaper to answer here (inferred). | Batching, group commit, SQLite settings. |
| #102 Scripted model (open) | Tests through `Runtime`: "recovery across runtimes, a recovering read runs its queue, abort settles dispatches it had not recovered, abort's closing results keep the history paired." New: abort of a live step (`hang`). | Add STOP tests: a store failure ends the actor and fails queued commands with the cause; old handle gets the new error; `runtime.instance(..)` returns a fresh actor that recovers. The public `StoreBackend` trait can inject `Fenced`/`Failed` without a new seam (inferred). Today no test reaches `reset` (no `Fenced` in `tests/durable.rs`; `instance/tests.rs` has no reset test). | `ScriptedModel`, `RuntimeBuilder::model`, the move of tests to `tests/it/`. |
| #36, #93 | OTel; tracing via the bus. | None found (grep for lease, recover, actor, passivation: no hits). | All. |

## ADR and glossary impact

ADRs at `8d4a1c5`: 0001 to 0006. None records the durable actor; its decisions live in the #48 Resolution.

| Item | Impact |
|---|---|
| ADR 0003 (`thiserror`, no foreign error types) | No change. A new variant (e.g. `InstanceStopped { agent, id }`, maybe with a cause) must hold only keelrs types; `StoreError` qualifies (`error.rs:56-57` wraps it with `#[from]`). `Error` is `#[non_exhaustive]` (`error.rs:9`), so adding a variant is not breaking. |
| ADR 0004 (keelrs drives `AgentRun`) | No change. Rule 3 ("resume build[s] a new `AgentRun` from the events") is what each fresh actor does. |
| ADR 0005, 0006 | No change. ADR 0006's built-in cache (`Seen`) is in-flight state; a stop loses it, as `reset` already does (`actor.rs:395`). |
| New ADR (recommended, inferred) | "An instance actor stops on lease loss; the runtime starts a fresh one." It reverses a recorded decision (#48), adds a public error, and fixes what a handle means. These are hard to reverse. |
| **Lease** (GLOSSARY.md:50-53) | "held by one runtime at a time until it is released or runs out; a runtime that has lost it writes nothing more to that stream." Still true. Optional addition: the instance that loses it stops. |
| **Abort** (GLOSSARY.md:138-140) | "Stopping an agent instance's work … _Avoid_: Cancel, kill, stop." A new "stopped" term would collide with this _Avoid_ list. Either pick another word for the actor's end, or narrow Abort's _Avoid_ entry. If abort also stops the actor, the definition must say so. |
| **Agent instance** (GLOSSARY.md:26-28) | "One conversation with an agent, addressed by an id; later dispatches to the same id continue it." Unchanged: the conversation outlives any actor. A stop is not a new instance. |
| **Receipt** (GLOSSARY.md:64-66), **Read**, **Settled** | Unchanged. A receipt is plain data, "readable from any handle to the instance that issued it" (`mod.rs:188-189`), so a fresh actor answers old receipts from the stream. A read failed by a stop is not a settle; "once settled it never changes" holds. |
| Fog term "`uid` incarnation" (#43, #46) | Already means a new conversation under a reused id (Flue). Do not name an actor's life an "incarnation" in the glossary (inferred). |
| Missing term | The actor itself has no glossary entry. If handles can go stale, users need a word for what ended (inferred). |

## PR #101 recommendation

**Amend it; do not close it.** Run a short grilling first.

Why not close:

- The STOP change is local to the failure path. At `8d4a1c5` the reset logic is `reset` (`actor.rs:389-402`) and its three callers (`:268`, `:334`, `:359`), plus `hold`'s re-acquire when `lease` is `None` (`:185-198`). The respawn is in `instance_with`'s `Entry::Occupied` arm (`runtime.rs:115-121`). The handle maps a gone actor to `Error::Run { Aborted }` (`mod.rs:134-138`, `:150-151`) and `abort()` to `Ok(())` (`mod.rs:119-121`); these change too.
- The rest of #97's "Public (built)" and most of "Crate-private (built)" is unaffected: the store contract and contract tests, `RuntimeBuilder`, `Run::step`, `Folded::apply`, write-ahead admission, `abort()`.
- STOP fixes #97's open defect: "A reset forgets the lease without releasing it" (#97 Open; PR #101 body line 69).

Why amend rather than merge then follow up:

- #102 is next and rewrites every instance test through `Runtime` (#102, Tests). Merging reset semantics first means writing the failure tests twice (inferred).
- `reset` has no test today, so nothing on `main` would pin it (inferred from `tests/durable.rs` and `instance/tests.rs` at `8d4a1c5`).

Why grill first: AGENTS.md lists "Concurrency: who owns, writes or shares state: a new task, actor …" and "Contracts: the public API, `Error` variants" as design decisions to propose and get approved. The map's rule is grill → task.

After the grilling: rewrite #97's body in place (Behaviour, Tests, Open, Metrics); record the reversal of the #48 bullet on the grilling ticket; rewrite the "Store failures already reset the actor" bullet on #49; note the broadcast owner on #92.

Alternative (if the user wants #101 merged now): merge as built (it matches #48 and passes its checks), and place the lifecycle grilling and its task before #102 on the map.

## Decisions for the grilling ticket

1. Which errors stop the actor: `Fenced`, `Failed`, a failed renew, the local deadline guard, and also `Held` at the first acquire?
2. Does `InstanceDataConflict` at recovery stop the actor, or does the actor stay up and refuse every command?
3. Does `abort()` stop the actor after it settles everything as `Aborted`, or does the actor keep serving?
4. Do callers see stops (a new `Error` variant, re-fetch through `runtime.instance(..)`), or do handles respawn privately, as #48 planned for passivation?
5. If callers see stops: what is the variant's name and shape (`InstanceStopped { agent, id }`, with or without the cause)?
6. What do queued commands and parked reads get: the real cause (`Error::Store(..)`), or the new variant?
7. What does `abort()` on a stopped handle return: `Ok(())` as today (`mod.rs:120-121`), or the new variant, given the stream may still hold unsettled dispatches?
8. What does `dispatch` on a stopped handle return, instead of today's `Error::Run { Aborted }` (`mod.rs:134-138`)?
9. Who respawns: only `Runtime::instance_with` on `is_closed()`, or also something else (a handle, a sweep)?
10. Does the actor release the lease before its inbox reads as closed, so a respawned actor does not meet `Held`?
11. Does a respawn back off after repeated failures, or retry on every `instance(..)` call?
12. Does the runtime evict closed entries from its instance map (#72)?
13. Does recovery stay lazy (first command, inbox as stash), or does a respawned actor recover at once?
14. Who owns the per-instance broadcast on #92: the actor (subscribers resubscribe from `seq`) or the runtime?
15. Is passivation (idle stop) built now on the same stop path, or kept in the fog with only its trigger left?
16. Which glossary word names the actor's end, given **Abort**'s _Avoid_ lists "stop" and the fog uses "incarnation"?
17. Is the change recorded as a new ADR?
18. Which public seams test it: a failing `StoreBackend` through the public trait, and #102's `ScriptedModel`?
19. Is #101 amended, or merged first with a follow-up task before #102?
