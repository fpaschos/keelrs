# How Rig orders a reply's tool calls by footprint, and what keelrs would need

Research for issue #31 (map: #29). It reads, from source, how Rig `0.46.0-rc.1` decides which tool calls of one model reply may run side by side, how that decision feeds restart and stop handling, and what each rig-tools tool declares. It then compares Flue's parallel batch and its per-path write lock. Sections 1 to 6 are facts with citations. **Section 7 is inference**: what keelrs's tool loop would need to do the same. It makes no decisions.

It builds on earlier notes and does not repeat them:

- [`rig-tools.md`](rig-tools.md) (on `main`) and [`rig-tools-rc1.md`](https://github.com/fpaschos/keelrs/blob/research/rig-tools-rc1/docs/research/rig-tools-rc1.md) (branch `research/rig-tools-rc1`): Rig's `Tool` API, and that `Tool::footprint` / `Touches` exists but the `rig-agent` loop does not consult it.
- [`sandboxes.md`](https://github.com/fpaschos/keelrs/blob/research/sandboxes/docs/research/sandboxes.md) (branch `research/sandboxes`): the shapes of `Touches`/`Scope`, that the file tools declare `Reads`/`Writes` of their resolved path, `write_if` as a hash compare-and-swap, and Flue's per-path lock. This note adds **who reads footprints and how they schedule**, which those notes do not cover.

House rule: this note names only keelrs, Rig and Flue. The ECS framework Rig's kernel is built on, and the agent-loop library Flue builds on, are described generically.

**Sources.**

- Rig: tag [`v0.46.0-rc.1`](https://github.com/0xPlaygrounds/rig/tree/v0.46.0-rc.1) (commit `6eff50a2f`), extracted with `git archive`. The local checkout is 21 commits past it and changes `rig-ecs/src/{turn,stage,tools,restore}.rs`, `rig-tools/src/{shell,workspace}.rs`; every line number below is the tag's. `CORE/`, `AGENT/`, `ECS/`, `HARNESS/`, `TOOLS/`, `RMCP/` stand for `https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/{rig-core,rig-agent,rig-ecs,rig-harness,rig-tools,rig-rmcp}/`.
- Flue: local checkout at tag `v2.3.0` (commit `deee93b4`, 2026-10-12). `FLUE/` stands for `packages/runtime/src/`, `DOCS/` for `apps/docs/src/content/docs/`. The agent-loop library Flue delegates batch execution to is not vendored in the checkout, so its internal ordering was not read.
- keelrs: `main` at `3bf76e3`.

## Summary

- **Only Rig's coding-agent kernel, `rig-ecs`, reads footprints.** `rig-core` defines `Touches` and passes a handler's footprint through every `Serve` wrapper; `rig-agent`'s engine and tool server never call it (no match for `footprint`/`Touches` in `crates/rig-agent`). `rig-harness` only registers tools on `rig-ecs`. `rig-rmcp` only maps server hints to annotations.
- **The rule is "wait for every earlier, unsettled, conflicting call of the same reply".** Not waves, not a dependency graph across replies. A call is never held back by a later one. Readers never wait for readers. A call held by a gate (e.g. awaiting permission) holds back later conflicting calls exactly like a running one. Calls start in reply order within a pass; results are committed in call order once all settle.
- **`Reads` vs `Writes`/`Independent` drives restart semantics.** Before a non-`Reads` call starts, the kernel writes a "started" mark to the session store and flushes it; if that fails, the call is refused. A `Reads` call gets no mark, so after a crash it simply runs again. A marked call that never settled is answered "may have partly run; check before running it again".
- **Trust levels.** A tool's own code may declare `Reads`; annotations from a remote server never make a call `Reads` (`from_hints` gives `Independent`). Unknown tools and calls not yet admitted count as `Writes(Anything)`.
- **rig-tools:** `read`/`search`/`list` read their resolved path; `write`/`edit`/`delete` write theirs; `move` writes both ends; `shell` keeps the default `Writes(Anything)`.
- **keelrs today** runs a reply's calls with unbounded `join_all`, and its `Tool` has no footprint or annotations. Two `edit`s of one file in one reply can lose an update, because rig-tools' `edit` does read-compare-write without a lock.
- **Flue** runs every batch in parallel with no footprints; `write`/`edit` chain on a per-resolved-path promise lock in dispatch order; `bash` and every other tool are unsynchronised. On restart, an unresolved ordinary call is never re-run, read-only or not.

## 1. The footprint type (`rig-core`)

`CORE/src/tool/footprint.rs` at rc.1:

```rust
pub enum Touches {                       // L27-L37
    Reads(Scope),      // "Reads what the scope names and changes nothing: run again after a restart."
    Writes(Scope),     // "Reads and may change what the scope names."
    Independent,       // "Changes nothing the other calls of its reply see ... It may read anything,
                       //  so it waits behind an earlier writer; it is never run again after a restart."
}
pub enum Scope { Paths(Vec<PathBuf>), Anything }                    // L51-L56

impl Scope {
    pub fn overlaps(&self, other: &Scope) -> bool                    // L62-L71
    pub fn normalized(self) -> Scope                                 // L77-L86
}
impl Touches {
    pub fn conflicts_with(&self, other: &Touches) -> bool            // L114-L118
    pub fn reads_only(&self) -> bool                                 // L121-L123
    pub fn normalized(self) -> Touches                               // L126-L132
    pub fn from_annotations(annotations: &ToolAnnotations) -> Self   // L138-L143
    pub fn from_hints(annotations: &ToolAnnotations) -> Self         // L151-L156
}
```

Facts:

- **Overlap** is component-wise prefix in either direction (`ours.starts_with(theirs) || theirs.starts_with(ours)`), so a directory covers what is under it; `Anything` overlaps everything (L62-L71).
- **Conflict** = at least one side writes, and the scopes overlap (L114-L118). `Independent` is treated as `Reads(Anything)` for this (`parts`, L158-L165). So `Independent` never conflicts with a reader but waits behind any writer. The module doc test (L5-L15) shows `Writes(a.rs)` vs `Reads(b.rs)`: no conflict; vs `Reads(src)`: conflict; vs `Independent`: conflict; `Reads(Anything)` vs `Independent`: no conflict.
- **Normalisation** folds `.` and `..` textually, follows no links, and turns any scope containing a **relative** path into `Anything` ("A relative path names no place a guard can check", L73-L86; `normal`, L89-L108). `..` at the root stays at the root. The type doc says a tool must declare canonical paths itself (L39-L48).
- **From annotations.** `ToolAnnotations::read_only()` is `read_only_hint.unwrap_or(false)` (`CORE/src/completion/request.rs#L157-L160`). `from_annotations`: read-only gives `Reads(Anything)`, else `Writes(Anything)`; meant for a tool's own code. `from_hints`: read-only gives `Independent`, else `Writes(Anything)`; meant for annotations "rig cannot check, such as those an MCP server sends ... Only a local declaration makes a call `Reads`" (L145-L156).
- `Touches` and `Scope` are `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`, snake_case (L25-L26, L49-L50): plain data.

Where it is declared and carried:

- `Tool::footprint` (`CORE/src/tool/contextual.rs#L67-L74`): `fn footprint(&self, args: &Self::Args) -> Touches`, defaulting to `Touches::from_annotations(&self.annotations())`. `annotations()` defaults to `ToolAnnotations::default()` (L59-L65), so an unannotated tool is `Writes(Anything)`.
- `Serve::footprint` (`CORE/src/serve/handler.rs#L100-L106`): `fn footprint(&self, kind: &EffectKind) -> Option<Touches>`, default `None` ("when it cannot say, such as for arguments that do not parse"). `ToolAdapter<T>` parses the call's JSON into `T::Args` and returns `Some(self.tool.footprint(&args))`, else `None` (`CORE/src/serve/adapters.rs#L546-L554`). Layers, erasure and sharing forward it (`CORE/src/serve/layer.rs#L130-L131`, `handler.rs#L153-L176`, `#L308-L309`, `#L365-L366`; tested by `a_footprint_passes_through_every_wrapper`, `CORE/src/serve/layer/tests.rs#L465-L483`). rig-tools' checkpoint layer forwards it too (`TOOLS/src/checkpoint.rs#L749-L750`).
- `feedback::not_finished(why: &str, started: bool, footprint: &Touches) -> String` (`CORE/src/tool/feedback.rs#L183-L210`): not started gives "Not run: {why}."; started and `reads_only()` gives "Stopped: {why}. It only read; run it again if you still need it."; started otherwise, `Independent` included, gives "Stopped: {why}. It may have partly run; check before running it again."

## 2. Who reads footprints

| Layer | Reads footprints? | Evidence |
|---|---|---|
| `rig-core` `serve/` | Carries them only; nothing schedules on them | §1 |
| `rig-agent` engine (`AgentRunner`, `AgentRun`) | **No.** Runs calls with `buffer_unordered(concurrency)`, default 1 | `AGENT/src/agent/runner.rs#L418-L429`, `AGENT/src/agent/engine.rs#L459-L482`, `#L520-L557`; no `footprint`/`Touches` anywhere in `crates/rig-agent` |
| `rig-agent` tool server | **No.** Registry of tools and handler tables | `AGENT/src/tool/server.rs` (no match) |
| `rig-ecs` (the kernel) | **Yes**: computes, normalises, schedules, and uses them for restart/stop | §3 |
| `rig-harness` | Registers tools on `rig-ecs`; documents the rule for plugin authors | `HARNESS/src/per_call.rs#L124-L127`, `HARNESS/PLUGINS.md#L372-L376`, `#L601-L628` |
| `rig-rmcp` | Maps server hints into `ToolAnnotations`, so a host applies `from_hints` | `RMCP/src/native.rs#L539-L554` |

`rig-ecs` describes itself as "the kernel of the rig coding agent: agents, their turns and their calls are [ECS] entities, and the calls run on [the framework's] task pools" (`ECS/src/lib.rs#L1-L2`). `rig-harness` depends on it (`HARNESS/Cargo.toml#L36`). For `rig-agent` users, rc.1's own guidance is the `tool_concurrency` knob: "Sequential execution follows call order and stops at the first terminating error; concurrent hooks and side effects may interleave. Committed history always retains call order" (`runner.rs#L418-L421`).

## 3. How `rig-ecs` orders a reply's calls

### 3a. Computing each call's footprint

- A tool's entity carries a default `Footprint(pub Touches)` (`ECS/src/tools.rs#L156-L175`). `Footprint::default()` is `Writes(Scope::Anything)`: "May change anything, right for any tool that does not say".
- Registration sets that default (`tools.rs#L238-L339`):
  - `add_tool_with<T: Tool>(tool, options)`: `Touches::from_annotations` (the tool's own code is trusted).
  - `add_open_tool` (answered by an observer, no handler): `Writes(Anything)` (L264).
  - `World::spawn_tool(definition, handler, options)` (runtime tools, such as a remote server's): `Touches::from_hints` (L303), "so a tool that says it only reads is never run again after a restart" (L284-L286).
  - `ToolOptions { pub footprint: Option<Touches> }` (L187-L199) overrides all of it and marks the tool `DeclaredFootprint` (L177-L185, L327-L337): "that footprint is each of its calls', whatever its handler says".
- `footprint_of(call, handler, tool) -> Footprint` (L346-L360): the tool's declared footprint if any; else the handler's `Serve::footprint` for this call's arguments; else the tool's default; else `Writes(Anything)` for an unregistered tool. The result is always `normalized()`.
- `compute_footprints` (L383-L413) recomputes this for every admitted, not-started, unsettled call "when Gate opens: after every Prepare edit of its arguments or handler". A plugin may insert `(Footprint(..), DeclaredFootprint)` on a call in Prepare and it stands (L399; `PLUGINS.md#L620-L626`: a plugin that knows `cargo fmt` only writes `src/` lets a read of `docs/` run beside it). Tested: `gate_sees_footprint_paths_normalized` (`ECS/tests/stages.rs#L1557`), `a_footprint_a_plugin_declared_on_a_call_stands` (L1505).

### 3b. The scheduling rule

`wait_in_order` (`ECS/src/turn.rs#L1496-L1531`), run on entering Gate right after `compute_footprints` (`ECS/src/stage.rs#L726-L734`):

```rust
let anything = Footprint::default();
for (state, calls) in &turns {
    let Some(reply) = state.reply else { continue };
    let mut earlier: Vec<&Footprint> = Vec::new();
    for call in reply_calls(reply, calls, &replies) {           // reply order
        let footprint = call.footprint.unwrap_or(&anything);
        let waits = earlier.iter().any(|before| footprint.0.conflicts_with(&before.0));
        let open = !call.started && call.outcome.is_none() && !call.settled;
        if open && waits != call.waiting { /* insert or remove `Waiting` */ }
        if !call.settled { earlier.push(footprint); }
    }
}
```

Facts read from it and its docs:

- **Scope of comparison is one reply.** `reply_calls` returns the calls whose parent effect is the turn's current reply, "in the reply's order", each replaced by its last retry (`SupersededBy`) (`turn.rs#L1466-L1494`).
- **Only earlier calls count.** Call *i* is marked `Waiting` while any call *j < i* that is **not settled** has a conflicting footprint. Running, waiting and held calls are all "not settled", so "a held call holds back the calls after it that conflict with it; a refused one, once it settled, does not. A call not yet admitted may change anything" (doc, L1496-L1501).
- **It is not wave scheduling.** `[Writes(a), Reads(b), Reads(a)]` starts calls 1 and 2 together, and call 3 when call 1 settles. A later non-conflicting call never waits for an unrelated earlier one. `PLUGINS.md#L601-L618` gives a worked five-call table (edit held for permission; two reads run at once; a read of the edited file and an `Independent` `task` wait for the edit). It also notes the consequence for `shell`: "every call after a held `shell` call waits, reads too: a later call may depend on what an earlier one changes (`mkdir x` then `ls x`). So a gate that cannot get an answer refuses at once rather than holding" (L619-L622).
- **`Waiting` is a gate filter.** `ReadyToStart` excludes `Waiting` (`stage.rs#L549-L561`), and Start only starts `ReadyToStart` effects (`stage.rs#L1076-L1077`). The mark is recomputed every pass when something moved (`run_if(moved_since_quiet)`, L730-L731), so it clears as soon as the blocking call settles.
- **Start order.** Within one pass, ready effects are sorted by effect id: "In the order they were spawned, such as a reply's calls in call order" (`stage.rs#L1128-L1130`). The reply's calls are spawned in call order (`spawn_reply`, `turn.rs#L1409-L1438`). Started work runs on the task pool concurrently (`stage.rs#L1226-L1228`).
- **Commit order.** `commit_replies` waits until every call of the reply settled, then appends results in call order; until then it writes a per-call "settled" mark for a restart (`turn.rs#L1533-L1560`).

### 3c. Restart, stop and retry semantics

- **Before a changing call starts**, Start writes a `Started` mark for every ready call whose footprint is not `reads_only()`, saves, and flushes the session log. If the flush fails, each such call is answered "Not run: the session can no longer be saved (...). Nothing changed." instead of starting (`stage.rs#L1100-L1127`, `#L1143-L1149`). `Reads` calls skip this write. `Independent` and `Writes` pay it.
- **On restore**, every pending call of the last reply is respawned. A call with a `Started` mark becomes `Interrupted::Started`, one with a `Settled` mark `Interrupted::Settled(result)`, one with no mark nothing (`ECS/src/restore.rs#L555-L575`). An unmarked call is ordinary work and runs again. In practice that is a `Reads` call, or one that never started. `answer_interrupted` answers `Started` with `not_finished(RESTART, true, footprint)` ("may have partly run") and `Settled` with its stored result, unless a plugin decided first or removed `Interrupted` in Prepare to rerun it (`restore.rs#L176-L236`).
- **On stop**, unfinished calls are answered with the same `not_finished` text from `started` and the footprint (`turn.rs#L497-L555`). The test `a_stopped_call_says_whether_it_ran_and_whether_it_may_have_changed_something` checks the three texts for a `Writes` call, a `Reads` call and a never-started call (`ECS/tests/stages.rs#L3160-L3200`).
- **So the three variants differ on two axes.** Concurrency: `Reads(s)` and `Independent` both run beside readers; `Independent` acts as `Reads(Anything)`. Restart: only `Reads` is re-run automatically; `Independent` and `Writes` are marked and reported as possibly partial. The approval layer also keys on it: a plugin example asks for a preview of every call that is not `reads_only()` (`PLUGINS.md#L556-L564`).

## 4. What each rig-tools tool declares

The crate doc says: "`read`, `search` and `list` read the path they name, the other file tools write theirs, and `shell` may change anything" (`TOOLS/src/lib.rs#L5-L8`). The paths are `workspace.resolve(base, path)`: joined onto `base` when relative, then `canonical` (links followed, `..` folded) (`TOOLS/src/workspace.rs#L127-L132`, `#L286-L294`).

| Tool | `footprint(args)` | Annotations | Source |
|---|---|---|---|
| `read` | `Reads(Paths[resolve(path)])` | `read_only(true)` | `TOOLS/src/read.rs#L232-L242` |
| `search` | `Reads(Paths[resolve(path or ".")])` | `read_only(true)` | `TOOLS/src/search.rs#L202-L211` |
| `list` | `Reads(Paths[resolve(path or ".")])` | `read_only(true)` | `TOOLS/src/list.rs#L138-L147` |
| `write` | `Writes(Paths[resolve(path)])` | | `TOOLS/src/write.rs#L93-L97` |
| `edit` | `Writes(Paths[resolve(path)])` | destructive, not idempotent | `TOOLS/src/edit.rs#L146-L157` |
| `delete` | `Writes(Paths[resolve(path)])` | | `TOOLS/src/delete.rs#L79-L83` |
| `move` | `Writes(Paths[resolve(from), resolve(to)])` | | `TOOLS/src/move_file.rs#L99-L105` |
| `shell` | not overridden: `from_annotations`, so `Writes(Anything)` | title only | `TOOLS/src/shell.rs#L174-L176` |

Tested in `TOOLS/src/tests.rs#L54-L105`, including `read("src/../a.rs")` gives `Reads(base/a.rs)` and `Shell::default().footprint(..)` gives `Writes(Scope::Anything)`.

**Why the footprint matters for `edit`.** `Workspace::write_if(path, bytes, held) -> io::Result<Option<Contents>>` defaults to "reads, then writes" with no lock (`TOOLS/src/workspace.rs#L97-L118`). `edit` first tries `write_if` against the text `Seen` last. On a hash mismatch, or with nothing seen, it falls back to `read_text`, applies the edits, and does a plain `write` (`TOOLS/src/edit.rs#L175-L214`). Two concurrent `edit`s of one file on separate `blocking` threads can therefore both read the same text and both write, and the second silently drops the first. Under `rig-ecs` the two calls conflict (`Writes` vs `Writes` on the same path), so the second waits and this cannot happen within a reply. Under `rig-agent` with `tool_concurrency > 1`, or keelrs's `join_all`, nothing prevents it.

## 5. Flue

- **Every batch runs in parallel.** The session builds its agent loop with `toolExecution: 'parallel'` (`FLUE/session.ts#L2285`). The docs say "Tool calls in one batch execute in parallel" (`DOCS/guide/subagents.md#L51`). No tool declares what it touches; there is no read-only flag in `FLUE/agent.ts`.
- **`withFileMutationLock(env: Sandbox, path: string, operation: () => Promise<T>): Promise<T>`** (`FLUE/agent.ts#L142-L171`). It is a `WeakMap<Sandbox, Map<string, Promise<void>>>` keyed by `env.resolvePath(path)` (falling back to the raw path when that throws). Each operation chains on the previous tail for that key, in dispatch order, and the entry is deleted when its chain drains. The doc comment gives the reason: parallel same-file mutations "would otherwise read one snapshot and last-write-wins — silently losing edits"; chaining "keeps cross-file parallelism while every read → modify → write transaction sees its predecessor's result, in dispatch order" (L119-L139).
- **Only `write` and `edit` take it** (`FLUE/agent.ts#L189`, `#L234`; the `edit` comment: "The whole read → replace → write transaction holds the lock"). "A `bash` command mutating the same file concurrently remains unsynchronized; a shell cannot take this lock" (L132-L133; also `DOCS/reference/sandbox-api.md#L336`). `read` takes no lock, so a `read` beside an `edit` of the same file may see either version.
- **Restart.** "An unresolved ordinary call is _not_ re-executed, because the runtime cannot know which of its side effects already happened; instead it settles with an explicit unknown-outcome error". Only `durable: true` tools (step-recorded) and delegated tasks resume (`DOCS/guide/durability.md#L37`, `#L67`). Flue has no equivalent of Rig's "a read is safe to re-run".
- **Result order** is set by the delegated agent-loop library, which was not read (see Sources). Batches commit atomically (`FLUE/session.ts#L3431-L3436`).

Side by side:

| | Rig `rig-ecs` | Rig `rig-agent` | Flue | keelrs today |
|---|---|---|---|---|
| Unit | one reply | one reply | one batch | one reply |
| Concurrency | all, minus conflicts | `tool_concurrency`, default 1 | all | all (`join_all`) |
| Declares effects | `Touches` per call | (ignored) | nothing | nothing |
| Same-file writes | ordered by footprint | unsafe at >1 | per-path lock, `write`/`edit` only | unsafe |
| Shell vs files | shell waits for / blocks everything | unsafe at >1 | unsynchronised | unsafe |
| Hold (approval) | holds later conflicting calls | n/a | n/a | n/a |
| Re-run on restart | `Reads` only | n/a | none (except durable tools) | n/a |
| Result order | call order | call order | atomic batch | call order (`answer_all`) |

## 6. keelrs today

- **`Tool`** is `{ name, description, schema, timeout, run }`, where `run: Arc<dyn Fn(Value, String) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync>` (`keelrs/src/tool.rs#L28-L45`). `Tool::new<Args, F, Fut, T, E>(name, description, run)` erases `Args` into `Value` (L59-L92). It has no annotations and no footprint.
- **The loop** handles `AgentRunStep::CallTools { calls }` with `join_all(calls.into_iter().map(|call| answer(call, &pinned)))`, then `run.answer_all(answers)` (`keelrs/src/agent/rig/mod.rs#L152-L159`; comment: "One reply's calls run concurrently; Rig commits them in call order"). `answer` looks up the pinned tool, checks the schema, and applies the timeout (L176-L208). `AgentRun::answer_all` takes the answers in any order and commits "as one user message in call order" (`AGENT/src/run/mod.rs#L1364-L1376`). So keelrs drives `AgentRun` itself (ADR 0004). It bypasses `rig-agent`'s `tool_concurrency` and is free to schedule the batch however it likes before `answer_all`.
- **The dogfood bridge** wraps rig-tools as keelrs tools through `fn rig<T: RigTool + Clone>(tool: T) -> Tool` (`examples/dogfood/src/coding.rs#L46-L69`). It says: "keelrs runs a reply's calls side by side and does not read Rig's footprints, so two calls that change one file can each write over the other; the implementer's instructions ask it never to send such calls in one reply" (L4-L6).

## 7. What keelrs would need (inference)

1. **Carrying a footprint fits `Tool` as plain data plus a callback.** `rig_core::tool::Touches` is public, serde, and already a keelrs dependency (`rig-core` is pinned at `=0.46.0-rc.1`), so it could be reused as is, or mirrored as a keelrs type to keep Rig out of the public API. A per-call footprint needs the arguments, so it would be a second erased callback beside `run`, something like `Arc<dyn Fn(&Value) -> Option<Touches> + Send + Sync>`. It would parse `Value` into `Args` again, as `ToolAdapter::footprint` does, with `None` on a parse failure. A builder (`Tool::footprint(|args: &Args| ..)`, `Tool::reads(..)`) keeps `Tool::new` unchanged. The bridge in `coding.rs` could then forward `RigTool::footprint(&args)` directly.
2. **Default for a tool without one: `Writes(Anything)`**, as Rig's `Footprint::default()`. With that default, a reply of only undeclared tools runs **sequentially in call order**, a behaviour change from today's `join_all`. A softer default (`Independent`) would keep today's parallelism among undeclared tools, but not their safety. This is the main decision for #35. A local `read_only` flag on `Tool` could map to `Reads(Anything)` as `from_annotations` does. Tools from a remote server would map to `from_hints` (`Independent` at best).
3. **The scheduler is small and local to the `CallTools` arm.** keelrs has no gates or holds, so Rig's rule reduces to this. Compute each call's normalised footprint (`Writes(Anything)` for a malformed or unknown call). Start call *i* once every earlier call *j < i* whose footprint conflicts with it has finished. Feed all answers to `answer_all`, which already orders them. One way is a per-call future that awaits the completion signals of its conflicting predecessors (for example, shared futures or a `Notify` per call) inside the same `join_all`. Unlike Flue's lock, this also orders `shell` against file tools and needs no cooperation from the tool body. Its cost is that a `shell` call serialises everything after it.
4. **Normalisation and paths.** Rig turns relative paths into `Anything` and trusts tools to declare canonical absolute paths. keelrs's own file tools (#26) and a future `Sandbox` (`sandboxes.md` section 4, item 5) would need to declare canonical sandbox paths, or every file call degrades to `Anything` and runs sequentially.
5. **Restart semantics come later but want the same data.** If keelrs makes agents durable in SQL, Rig's split maps directly: persist a "started" mark before any non-`Reads` call, re-run unmarked (`Reads`) calls on recovery, and answer marked ones with the `not_finished` wording. Flue's stricter rule, never re-running an unresolved call, is the alternative. Either way the footprint should be plain serialisable data on the call, which `Touches` already is.
6. **Not needed for ordering:** Flue-style path locks inside `write`/`edit`. With footprint ordering they are redundant within a reply. They would still protect writes across replies or across agents sharing a sandbox, which footprints do not cover (Rig compares only calls of one reply).
