# rig-harness: sessions, progress, resume (and what fits keelrs)

Source: `/Users/fpaschos/Dev/Work/sapp/rig` at 0.46.0-rc.1. All paths below are relative to `rig/crates/` unless noted. Read-only investigation.

## 0. Framing: rig-harness is a Bevy ECS app, not an actor system

- rig-harness is "a Bevy app on the rig_ecs kernel" (`rig-harness/src/lib.rs:1-7`). Its Cargo deps are bevy/bevy_ecs/bevy_app plus `rig-ecs` (kernel), `rig-memory` with `fs` (the session store), and `rig-harness-protocol` (launcher protocol) (`rig-harness/Cargo.toml:19-49`).
- The whole app is one loop: run a frame, then sleep until a `Wake`, a deadline or 1 s (`rig-harness/src/host/runner.rs:1-8, 59-74`). Every schedule runs single-threaded on the main thread (`runner.rs:83-93`).
- So there is no Ryhl-style actor per agent. Agents are ECS entities with a `Conversation` component; background work (tool output, file watches) runs on task pools and talks back through crossbeam channels plus `Wake` (`progress.rs:59-73`, `watch.rs:98-108`).
- Most session logic the question asks about lives below rig-harness: the log format and writer in `rig-memory`, the save/restore systems in `rig-ecs` (`session_log.rs`, `restore.rs`). rig-harness picks the directory, opens the store, marks lifecycle files, and deletes/rewrites stopped sessions (`rig-harness/src/host/session.rs:1-6`).

## 1. What a session records, and where

### On-disk layout (default `DirStore`)
- A session is a directory under the rig home, named by `SessionId` from the launcher env or freshly generated (`host/session.rs:173-185`).
- Store contents: `logs/<name>.jsonl` and `files/<name...>`, blobs under `files/blobs/`, lock file `.rig-memory.lock` (`rig-memory/src/dir.rs:1-4, 12-28`). Each change locks the directory lock; reads take no lock (`dir.rs:30-33`).
- Plain files beside the store, owned by the protocol's `SessionDir`: markers `started`, `saved`, `deleting`, the text log `agent.log`, `launcher.lock`, working directory, `name`, and `switch` (`rig-harness-protocol/src/lib.rs:504-511, 534-591, 669-671`).
- `Scratch` (non-store files): `spill/` (truncated tool output kept whole), `images/`, `agent.log` (`host/session.rs:142-169`).
- The store is pluggable: `StoreOpener` defaults to `DirStore` and a plugin can swap it (`host/session.rs:64-90, 136-140`). `rig-sqlite` ships `SqliteSessionStore`, an `AsyncStore` with tables `rig_memory_logs(name, len)`, `rig_memory_log_chunks(name, start, bytes)`, `rig_memory_files` (`rig-sqlite/src/memory.rs:1-45`); rig-memory says rig-postgres does the same (`rig-memory/src/lib.rs:29-30`).

### Format: append-only JSONL, one log per agent (+ a session log)
- Format `rig-session/4`: one JSON-lines log per agent, named by agent id, plus the session's own log `session` (`rig-memory/src/format.rs:1-3, 38-42`).
- Every line = a `Stamp` + one record. Stamp: `n` (global order across all logs of the session), `seq` (dense per-log, header = 0), `t` (ms epoch) (`format.rs:44-59`).
- Record types: `header` (format, log name, `parent` log, `lineage`, `rewrites`), `state` (key/value, latest wins, `null` removes), `retired`, and conversation changes `push`, `splice`, `halt`, `aside`, `reset`, each with `by` (author) (`format.rs:3-12, 162-215, 235`). Example lines at `format.rs:21-25`.
- Every string in a line is redacted for secrets on write (`format.rs:18-19, 367-396`).
- The "events" are conversation diffs (`rig_core::conversation::Change`) plus keyed state snapshots of reflected components, not a semantic event catalog. A component opts in with `#[reflect(Component, Saved)]`; each change is logged under its type path or a stable key, agent components to the agent log, resources to `session` (`rig-ecs/src/session_log.rs:35-53, 75-110`).
- `reset` is how snapshots appear inside the log: if a conversation does not continue the log's lineage, the writer queues one `Reset { state: whole conversation }` instead of the diffs (`rig-memory/src/writer.rs:164-193`).
- Side snapshot: `meta.json` (`SessionMeta`: title, labels, updated, model, ...) is an explicitly derived cache "the logs hold the same facts; this is the cache a listing reads without folding them" (`rig-memory/src/meta.rs:1-40`). rig-basics rewrites it on every `ConversationChanged` (`plugins/rig-basics/src/sessions.rs:4-8, 320-345`).
- Text log `agent.log` (tracing output, panics) is separate and not part of the resumable state (`host/session.rs:723-743`).

### Append-only vs snapshot
- Records are only appended; restore reads the logs back (`rig-ecs/src/session_log.rs:11-13`).
- In-place mutation exists only for stopped sessions: `rewrite` (e.g. redaction) gives the log a fresh lineage and records who rewrote it in the header (`format.rs:10-12, 181-189`; `host/session.rs:519-573`), and `edit_stopped` reads/edits/saves a whole `Session` (`host/session.rs:466-494`). Both refuse the running session and hold the launcher lock (`host/session.rs:504-517`).
- Forking an agent copies a log prefix up to a turn boundary into a new log with a new header/lineage, keeping original stamps (`writer.rs:284-344, 517-552`).

## 2. Who writes, and when

- Single writer object per process: `SessionLog(Writer)` resource; `Writer` is `Arc<Mutex<Inner{store, book}>>`, clones share it (`rig-ecs/src/session_log.rs:125-129`; `rig-memory/src/writer.rs:406-417`).
- Two-phase: changes accumulate in each `Conversation`'s outbox; saving moves them into per-log `pending` byte buffers (`writer.rs:171-237`), and `flush` appends each log's pending bytes in one write (`writer.rs:350-403`).
- When saves happen: between stages of every agent pass, at end of frame, before a tool that may change something starts, and when a turn is stopped (`session_log.rs:7-12`; schedule in `rig-ecs/src/lib.rs:117-140`). Writes happen in `Last` in the `WriteLogs` set (`session_log.rs:326, 364-376`). Each save is also announced as a `ConversationChanged` ECS message (`session_log.rs:191-200, 211-222`) which is the live observation hook (see section 3).
- Flush order: subagent logs before parent logs so a parent never references an unstored child message; the `session` log once any is started (`writer.rs:367-376, 559-563`).
- Nothing is logged before restore finishes: `Writer::start` is called at the end of `restore_session`, so restoring itself writes nothing (`writer.rs:460-467`; `rig-ecs/src/restore.rs` `restore_session` ends with `world.flush(); log.start();`, around lines 353-355).
- A session nobody used is not kept: a value an agent was spawned with does not start its log (`session_log.rs:70-74, 96`); the harness marks the dir `saved` only after a log write AND a user/assistant message exists (`host/session.rs:390-412`), and deletes an unsaved dir on exit (`host/session.rs:422-464`).
- Concurrency control is optimistic, at the store: `append_log(name, len, bytes)` appends only if the log is still `len` bytes; otherwise `LogWriteError::Changed`, nothing written; the check+append is atomic across processes (`rig-memory/src/store.rs:1-8, 85-95`; sqlite does it in a transaction, `rig-sqlite/src/memory.rs:79-120`). The writer tracks each log's `len` (`writer.rs:39-41, 387-398`).
- Process-level exclusivity: the launcher holds `launcher.lock` while a session runs; `runs_elsewhere()` checks it; edits/deletes of other sessions take it (`rig-harness-protocol/src/lib.rs:588-619`; `host/session.rs:504-517, 644`). One session per process; switching = write `switch` and exit with reload code (`plugins/rig-basics/src/sessions.rs:12-14`; protocol `lib.rs:666-671`).
- Failure policy: a failed write stops all further logging (`failure` latched), surfaced once as an error `Notice` (`writer.rs:47-53, 390-397`; `session_log.rs:364-376`). No fsync: "written is not yet proof against a power cut" (`writer.rs:562-563`).

## 3. How progress reaches a watcher

- Nothing about live progress is persisted. `ToolProgress(String)` is an ECS component on the call entity (latest line), drawn by the TUI and written to stderr by `--print` (`rig-harness/src/progress.rs:12-17`).
- Mechanism: a tool gets a `Live` callback that sends text over an unbounded crossbeam channel and calls `wake.wake()` (`progress.rs:56-73`). A system in `Stage::Arrive` drains it each pass, keeps a bounded tail, and updates `ToolProgress` + `IfStopped` (`progress.rs:115-143`). `IfStopped` is what the call answers if cancelled: output so far + `[stopped before it finished]` (`progress.rs:101-112`).
- `per_call.rs` wires this: tools registered with `add_live_tool_per_call` get a `CallOutput` and the `Live` in their `CallPlace` (`per_call.rs:126-147, 328-353`); `CallPlace` also carries the spill dir (`per_call.rs:46-66`).
- Committed-history observation is the `ConversationChanged` message (agent entity + `ChangeRecord`), emitted on every save (`rig-ecs/src/session_log.rs:191-200`); consumers like the sessions listing read it (`plugins/rig-basics/src/sessions.rs:327, 339`).
- Logs/warnings: a tracing `Layer` (`PassOn`) forwards WARN/ERROR events into a bounded(256) crossbeam channel (`LoggedQueue`) for plugins; overflow only reaches `agent.log` (`host/session.rs:847-884`).
- `timings.rs` is in-memory profiling: a tracing layer times Bevy system spans into `SystemTimings` (`Arc<Mutex<..>>`), per plugin, last frame + totals; not persisted (`timings.rs:1-11, 53-58, 203-281`).
- `watch.rs` is not session watching: it is filesystem watching (`FileWatch` component -> `FileChanges` messages), armed before listing so no file is missed (`watch.rs:1-17, 140-170`).
- There is no external subscribe/tail API for another process; observation is in-process ECS messages/components. Another process can only read the store/`meta.json`.

## 4. What survives a crash, and how resume works

- Survives: whatever `flush` appended (no fsync) (`writer.rs:562-563`), `meta.json`, marker files, spill files. Not surviving: `ToolProgress`, `CallOutput`, timings, held approvals ("nothing held or approved before the restart is kept", `rig-ecs/src/restore.rs:8-10`).
- Open is strict: the whole session is read and folded; any unreadable record or other format refuses the run before anything is written (`rig-memory/src/format.rs:61-102`; `writer.rs:424-458`; `host/session.rs:277-301, 343-370`). An older format is migrated via a chain, with saved values type-checked, then re-read (`host/session.rs:363-377`).
- Torn tail repair: bytes after the last newline, or a final non-JSON line, are not a line (`format.rs:398-426`). On open they are moved to `recovered/<log>-<sha256>.part` and the log is replaced with the prefix (crash between the two steps redoes it next open) (`writer.rs:429-449, 580-619`). Repairs surface as notices (`restore.rs` `restore_session`, ~lines 292-296).
- Restore (`PreStartup`): spawn each non-retired agent with its folded `Conversation` and saved components, relink parent/child from headers, restore saved resources, then start logging (`rig-ecs/src/restore.rs:255-363`). Plugins re-arm their own obligations via `On<Insert<T>>` observers on restored components (`restore.rs:12-14`).
- Reconcile (`Startup`): settles half-done work "with ordinary records, so the next restore finds nothing left to fix" (`restore.rs:1-3, 548-563`). Per agent: tool calls without results are respawned as `Restored` work under a fresh reply id; a conversation awaiting the model gets a new model call; inbox messages are put back (`restore.rs:565-626`).
- Write-ahead for side effects: before any call that may change something (footprint not read-only) starts, a `rig.call` aside `Started{call}` is added to the conversation and the log is flushed; if the flush fails the call is refused: "a restart could not tell that it may have run" (`rig-ecs/src/stage.rs:1114-1135`; marks defined `restore.rs:74-95, 107-122`). When a call settles while its reply still runs, the mark becomes `Settled{call, result}` (`rig-ecs/src/turn.rs:1495-1507`).
- On restore those marks become `Interrupted::Started` (answered with "the session restarted" error) or `Interrupted::Settled(result)` (answered with the recorded result), unless a plugin decides/holds first (`restore.rs:69-72, 178-237, 600-608`).
- Exit is a clean checkpoint too: `StopTurnsOnExit` stops running turns "and leaves them for the restart, unended", before `WriteLogs` (`rig-ecs/src/lib.rs:116-120`).
- `AppStart` tells plugins why the process started: `New`, `Resumed`, `Reloaded`, `Restarted` (rolled back to previous build) (`host/session.rs:196-201, 233-241`; `rig-harness-protocol/src/lib.rs:177-188`). `started` marker is written at end of first frame (`host/session.rs:414-420`).
- Crash-safe delete: mark `deleting` (hides from listings), destroy store, remove dir; next start finishes cut-short deletes (`host/session.rs:604-721`; `store.rs:104-106` "idempotent").

## 5. "Successor" is not session handoff

- `successor.rs` is effect supersession inside one run: any plugin may spawn a successor effect linked by `SupersededBy` (e.g. a watchdog `resend`ing a stalled model call); the old effect settles as superseded with no outcome; the owner follows with `follow_successor` (`rig-harness/src/successor.rs:1-28, 38-97`). Nothing is persisted about it.
- Process-to-process handoff is the launcher protocol: the agent writes the next `SessionId` (or nothing for new) to `switch` and exits with `RELOAD_EXIT_CODE`; the launcher reads/removes it and starts the next process (`rig-harness-protocol/src/lib.rs:666-671`; `plugins/rig-basics/src/sessions.rs:12-14`). An unsaved session left via switch is cleaned by the launcher (`host/session.rs:422-431`).

## 6. Actor/task structure (summary)

- One OS process per session, guarded by `launcher.lock`. One Bevy world, single-threaded executor (`runner.rs:83-93`). One `Writer` (shared via `Arc<Mutex>`) per world. Background I/O on Bevy task pools; results come back via crossbeam channels + `Wake` (`progress.rs:59-66`, `watch.rs:98-108`). The only "multiple writers" case is cross-process, and it is defended by store-level length CAS (`store.rs:85-95`).

## 7. keelrs: take / avoid

Take:
1. Per-agent append-only log with a header record (format version, agent id, parent id, lineage) and a stamp on every record: global `n` + per-log dense `seq` + `t` (`format.rs:44-59, 162-179`). Maps directly onto keelrs's actor-as-single-writer: the actor owns `seq`.
2. Optimistic append guarded by expected length/seq (`store.rs:85-95`, sqlite impl `rig-sqlite/src/memory.rs:79-120`). In sqlx: `INSERT ... WHERE (SELECT max(seq)) = expected` in one transaction, or a unique `(agent_id, seq)` constraint. Cheap fencing against a zombie actor.
3. Write-ahead mark before any side-effecting tool, flushed before execution, refuse to run if the flush fails (`stage.rs:1114-1135`); on resume, map Started -> "interrupted, may have run" and Settled -> replay recorded result (`restore.rs:178-237`). This is the single most valuable idea for keelrs resume.
4. Reconcile at startup by emitting ordinary events, so a second restore finds nothing to fix (`restore.rs:1-3`). Resume should never be a special code path that leaves the log inconsistent.
5. Restore without logging: replay state, then start the writer (`writer.rs:460-467`). Do not re-emit events while rebuilding actor state.
6. Strict open + torn-tail quarantine (`writer.rs:424-449, 580-619`); for sqlx this becomes "refuse unknown event version / migrate first" (`host/session.rs:363-370`).
7. Latch-on-failure: once a write fails, stop and surface it, never silently skip events (`writer.rs:390-397`).
8. Keep a derived listing/summary row (`SessionMeta`) as an explicit cache rebuilt from events, not a source of truth (`meta.rs:24-29`).
9. Separate ephemeral progress (latest-line, bounded tail, `IfStopped` partial result) from durable events (`progress.rs:12-17, 101-112`); only the final tool result is durable.
10. Flush children before parents so a parent never references an unstored child event (`writer.rs:559-563`).
11. Redact secrets at the serialization boundary (`format.rs:367-396`).
12. `retired` record so a resume does not resurrect finished subagents (`format.rs:204-215`; `session_log.rs:202-209`).

Avoid / does not fit:
- The ECS frame/schedule model (save sets in `Last`, `Changed<T>` detection) and the global `Arc<Mutex<Writer>>`: in keelrs each actor should own its own writer/sequence; no shared mutex across agents.
- Conversation diffs (`push/splice/reset`) as the event vocabulary: keelrs's canonical events are semantic (ADR 0004); rig's records are storage-level deltas of `Conversation`. Use rig's idea of a whole-state `reset`/snapshot record only as a compaction checkpoint.
- Reflection-driven "any component marked Saved is persisted" (`session_log.rs:35-53`): implicit, type-path keyed; keelrs should have an explicit, versioned event catalog.
- In-process-only observation (ECS messages, crossbeam): keelrs needs a broadcast/watch from the actor plus a "read from seq N then tail" API for external observers; rig has no equivalent.
- No fsync / "written is not durable" (`writer.rs:562-563`): with sqlx commit semantics this is free; do not copy the weaker guarantee.
- File markers + launcher lock + reload-exit-code handoff (`switch`): process-oriented CLI plumbing; in keelrs use a lease/owner column or the seq CAS instead.
- `successor.rs` supersession is about re-sending a stalled effect; useful as a pattern only if keelrs adds retries (record `superseded_by` as an event), not for session handoff.
