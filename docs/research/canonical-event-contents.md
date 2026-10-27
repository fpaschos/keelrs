# Canonical events for keelrs: what they hold, and what a user sees mid-run

Date: 2026-10-27. Sources read at:

- fluex `../fluex` (working tree), on Flue 2.3.0 (`fluex/node_modules/.pnpm/@flue+runtime@2.3.0…`)
- Flue `../flue` at `deee93b4` (v2.3.0)
- Rig `rig-agent` / `rig-core` 0.46.0-rc.1 from the cargo registry (`~/.cargo/registry/src/index.crates.io-*/rig-{agent,core}-0.46.0-rc.1`). This is the version keelrs pins in `keelrs/Cargo.toml:15-16`.
- DeepSeek Harness `github.com/deepseek-ai/deepseek-harness` at `22ddee37` (2026-10-23), shallow-cloned for reading. Paths below are relative to that repo.
- keelrs `../keelrs` (working tree)

Paths are relative to each repo's root unless they say otherwise.

## Question

What should keelrs's canonical events (ADR 0004) contain, and what can a user see of a run while it is still running? The design must cover four things: an envelope, an event catalog for one agent instance, durability (a sqlx store and resume) and live observation (subscription and tracing). Flows come later, and the envelope must already fit them.

## Short answer

- **fluex has no canonical log of its own.** It reads Flue's live `observe()` stream and writes a lossy, fluex-shaped `runs/<instanceId>/events.jsonl` for people to read (`packages/flue-tools/src/tool-log.ts:47-54`, `:271-276`). The durable truth is Flue's `data/flue.db`. So the data fluex supplies is mostly Flue's two layers, plus what fluex added on top: tool-call numbering, hook runs, blocks, settle totals, and the "what an operator wants in a run folder" view (`docs/research/observability.md:10-17`, `:25-31`).
- **Flue and DeepSeek Harness (DSH) agree on the core idea.** There are two streams:
  1. a **durable, append-only, sequenced record log** that is the only source of model context and resume;
  2. a **live, in-process event stream** for watching the run. Most of it is never persisted: text deltas in DSH, tool-argument deltas and turn requests in Flue.
- **Both write ahead of side effects.** A tool call is durable before the tool body runs. A tool's result is durable before observers are told about it. A crash between a call and its result becomes an explicit "outcome unknown" tool result on resume; the tool is never re-run blindly.
- **keelrs today** writes nothing until a dispatch settles (`keelrs/src/agent/instance.rs:154-176`). A failed or aborted run drops every message and tool call it produced (`:162-168`). A live observer has nothing to see, and `tracing` shows only rig-core's per-call `chat` span.

## 1. Side-by-side catalog

### 1a. Envelope

| Field | Flue durable record | Flue live event | DSH session event | fluex `events.jsonl` | keelrs today |
|---|---|---|---|---|---|
| format version | `v: 1` per record (`conversation-records.ts:7`) | `v: 3` (`types.ts:1410-1415`) | per-generation log header and format migrations (`docs/architecture.md`, "Session log") | none | none |
| record id | `id` (`conversation-records.ts:8`); deterministic for idempotent records, e.g. `record_tool_outcome_<msg>_<call>` (`session.ts:2610-2613`) | none | none (identified by `seq`) | none | none |
| sequence | per **batch**: `seq` / offset in `flue_conversation_stream_batches` (`runtime/conversation-stream-store.ts:128-140`); per-block delta `sequence` (`conversation-records.ts:175`) | `eventIndex`, per context and not durable (`types.ts:1386-1388`) | `seq`: monotonic and contiguous per session (`packages/core/session/src/types.ts:281-284`, `:505-506`) | none | none |
| timestamp | `timestamp` ISO (`conversation-records.ts:13`) | `timestamp` ISO | `time`, epoch ms (`types.ts:507-508`) | `ts`, copied from the Flue event | none |
| scope ids | `conversationId`, `harness`, `session` (`:10-12`) | `instanceId`, `agentName`, `conversationId`, `session`, `parentSession`, `taskId`, `harness` (`types.ts:1379-1390`) | session id is the log itself; children link through `parentSession` in the header (`types.ts:112`) | the instance id is the directory name | none |
| causation / correlation | `submissionId`, `attemptId`, `operationId`, `turnId` (`:14-17`); `parentId` on message records builds a tree (`:96`, `:153`) | `submissionId`, `operationId`, `turnId`, `toolCallId` | `turn` and `step` numbers in `data`; `sourceEventSeqs` on surface events (`types.ts:475-490`) | `toolCallId`, `n` (call ordinal), `turnId` | `SubmissionId` on `Settled` only (`event.rs:18-21`) |
| compatibility marker | reserved signal vocabulary (`conversation-records.ts:120-143`) | none | `ignorable?: true`. A reader **refuses** to load a log that has an unknown event type without this marker (`types.ts:509-520`; `known-event-types.ts:8-21`) | none | none |

### 1b. Event types for one agent instance

The rows are concepts. Each cell gives the harness's event and its fields; `—` means the harness has no such event.

| Concept | Flue durable record (`conversation-records.ts`) | Flue live `observe()` (`types.ts:1209-1370`) | DSH durable session event (`packages/core/session/src/types.ts:287-436`) | fluex `events.jsonl` (`tool-log.ts`) | keelrs `Event` (`event.rs`) |
|---|---|---|---|---|---|
| instance created | `conversation_created {kind root\|task\|action, affinityKey, createdAt, initialData?, uid?, parentConversationId?, taskId?, agent?}` (:40-91) | `agent_start` | session header plus `session/end-seed` (:408-436) | — | — |
| input admitted | `user_message {messageId, parentId, content[text\|attachment]}` (:93-98); submission row (below) | `submission_queued {submissionId, kind}` (:1306-1313) | `user/message` (UserMessage, `source` = human, inject, or goal) (:305-312) | — | `Message(user)`, only after settle |
| framework / context injection | `signal {messageId, parentId, signalType, tagName?, content, attributes?}` (:100-108) | — | `developer/message`, `system/message {turn, step, message}` (:313-335) | `context` note | — |
| attempt / run start | (`attemptId` in the envelope) | `submission_running {attemptCount, maxAttempts}` (:1314-1325) | `turn/start {turn}` (:294) | — | — |
| model call start | `agent_start_run` (start-hooks checkpoint) (:354-356) | `turn_start {turnId, purpose}`; `turn_request {request: systemPrompt, messages, tools, model info}` (live only) | `step/start {turn, step}`; `request/header {header, reason initial\|resume\|change\|series}`; `request/context` (:303, :394-407) | — | — |
| assistant streaming | `assistant_message_started {messageId, parentId, modelInfo, responseMetadata?}`; `assistant_text_started/delta/completed {blockId, blockIndex, sequence, delta, deltaCount, textSignature?}`; same for reasoning, plus `encrypted?` and `redacted?` (:150-211) | `message_start`, `text_delta {text}`, `thinking_start/delta/end`, `message_end` | **not durable** while streaming: `agent/assistant-stream` frames are process-local (`docs/architecture.md`, "Turn flow"). At settle, the whole timed stream is embedded in `assistant/message.stream` | — | — |
| assistant turn complete | `assistant_message_completed {messageId, stopReason, usage, error?}` (:224-230) | `turn {turnId, durationMs, request info, response{id, model, usage, finishReason}, isError}` | `assistant/message {turn, step, message, stream: AssistantStreamRecord[], usage?, interrupted?}` (:349-357) | `turn {turnId, durationMs, isError, usage}` (:136-148) | `Message(assistant)`, only after settle |
| failed / retried model attempt | (error on `assistant_message_completed`) | `turn {isError}`; `submission_recovery` | `assistant/attempt {turn, step, stream}`: kept without adding to model history (:358-363) | — | lost |
| tool call requested | `assistant_tool_call {messageId, blockId, blockIndex, toolCallId, name, arguments, thoughtSignature?}` (:213-222) | `toolcall_delta {argumentTextDelta}` (live only, :1233-1242); `tool_start {toolName, toolCallId, args}` | `tool/call {turn, step, callId, name, arguments: raw JSON string}` (:364-369) | `tool_start {n, toolCallId, tool, args (clipped at 2000 chars)}` (:79-95) | inside the final assistant message only |
| tool result | `tool_outcome {assistantMessageId, toolCallId, toolName, isError, content, output?, terminate?, durationMs?}` (:232-263) | `tool {toolName, toolCallId, isError, result, durationMs}` plus detail `{args, effectiveResult, errorInfo, origin}` (:1150-1160) | `tool/result {turn, step, message, error?{name, code, reason}, meta?}` (:370-393) | `tool {n, isError, failed, durationMs, exitCode?, bytes, output tail}` (:97-134) | inside the final user message only |
| tool batch committed | `tool_results_committed {assistantMessageId, parentId, outcomeIds}`: same batch as state writes (:265-270) | — | `step/end` (:305) | — | — |
| durable tool step memo | `tool_step_settled {toolCallId, toolName, stepName, value}` (:405-411) | — | `job/registered`, `job/settled` (long-running tools) | — | — |
| hook / policy | `agent_finish_cycle` (:370-372) | — | `hook/invoked`, `hook/result`, `approval/asked`, `approval/decided` | `hook_run {event, command, exitCode, durationMs}`; `hook`/`blocked` notes (:186-241) | — |
| app state | `state_write {name, value}` (:339-343); `message_data_write {name, data}`; `message_metadata` | — | `todo/write`, `goal/change`, `plan/mode` … | — | — |
| compaction | `compaction {entryId, parentId, summary, firstKeptEntryId, sourceLeafId, tokensBefore, usage?}` (:272-282) | `compaction_start`, `compaction` | `compaction/start\|summary\|prune\|end`; the surface `replace` op (:464-473) | — | — |
| child / subagent | `child_session_retained {child: {conversationId, harness, session, taskId, parentToolCallId?}}` (:284-316) | `task_start`, `task` | child session with `parentSession`; `subagent/*` | — | — |
| settled | `submission_settled {submissionId, outcome completed\|failed\|aborted, error?}` (:318-329) | `submission_settled` (same fields) | `turn/end {turn, reason}`, where the reason is a cancel cause, an error, or `interrupted` (:296-302, `:194-238`) | `settled {outcome, error, turns, tools, errors, blocked, hooks, usage, minutes}` (:153-181) | `Settled {submission_id, result: Ok(text)\|Err(Outcome)}` |
| log / diagnostics | — | `log {level, message, attributes}`, `idle` | `llm/retry`, `llm/retry-started` | `log` | — |

## 2. Per harness: who writes, when, where, how it is watched, and what is rebuilt

### fluex

- **Who writes:** `ToolLog.record`, which subscribes to Flue's `observe()` (`packages/flue-tools/src/tool-log.ts:69-184`, `:411-415`). It is mounted once per process (`mount-tool-log.ts:10-17`).
- **When:** after the fact. It reacts to Flue live events that Flue emits only after its own durable write (see Flue below). It writes synchronously with `appendFileSync` (`:271-276`). It numbers calls and keeps open-call state in memory, then drops it at settle (`:180`).
- **Where:** `runs/<instanceId>/events.jsonl`. Field names are fluex's own and stable across Flue upgrades (`:53`, `docs/research/run-cli.md:19-29`). Outputs and long arguments are clipped (`MAX_FIELD = 2000`, `:34`, `:352`, `:363-370`).
- **How it is watched:** the console line per call (`[id] #n bash(cmd) ✓ exit 0 · 0.1s`, `:262-269`); following the file (`docs/research/run-cli.md:41-46`); the CLI reads Flue's HTTP update stream for text and thinking (`packages/cli/src/line-presenter.ts:19-55`, `agent-client.ts:121-126`). Flow status is in memory only and lost on restart (`packages/server/src/flows/runs.ts:10`, `sdk/src/wire.ts:4-12`).
- **What is rebuilt:** nothing. It is an operator log, not a source of truth. fluex relies on Flue for history, resume and re-attach (`docs/research/observability.md:14`, `run-cli.md:43-46`).
- **Ideas fluex adds:**
  - per-instance call ordinals (`n`);
  - `failed` kept apart from `isError` (a non-zero exit code is a failure even when the tool did not throw) (`:104`);
  - output byte counts plus a clipped tail;
  - a settle summary of turns, tools, errors, blocked calls, hooks, tokens, cost and minutes;
  - a run id that sorts and is safe in paths (`sdk/src/run-id.ts:1-12`);
  - flow steps as instance ids `<runId>-<step>` with idempotency key `<runId>:<step>` (`server/src/flows/runs.ts:19-28`).

  These are the parts relevant to a later flows envelope.
- **What fluex wanted and Flue lacked:** the full prompt (`turn_request`) is "never persisted" (`observability.md:13`). There is no listing API (`:14`). Live `observe()` is in-process only, with no replay (`:12`).

### Flue

- **Who writes:** the `Session` writes conversation records through `ConversationRecordWriter` (`session.ts:875-889` envelope, `:903-915` `enqueueCanonical`). The submission coordinator writes `submission_settled` (`runtime/agent-submissions.ts:749`, `:1182`).
- **When, relative to side effects:**
  - Text and reasoning deltas are *enqueued*, then coalesced for up to 1 s (`conversation-writer.ts:34-41`, `:151-187`). The live `text_delta` event is emitted only **after** the batch holding it is durable (`session.ts:2374-2390`: `enqueueCanonical(records, () => this.emit(...))`).
  - Block boundaries and message completion flush at once (`session.ts:2396-2399`, `:2519-2548`).
  - `assistant_tool_call` is appended and awaited at `toolcall_end`, **before** the tool runs (`session.ts:2476-2492`).
  - `tool_outcome` is appended after the tool runs and before the live `tool` event (`:2590-2640`). The `tool` event is held back further, until `tool_results_committed` lands (`:2641-2665`, `:2700-2722`).
  - State writes ride in the same atomic batch as the commit (`:2690-2712`, `conversation-records.ts:331-343`).
  - Rule: **durable, then observable**.
- **Where:**
  - One stream per agent instance, in SQL. `flue_conversation_streams` holds the head offset, producer id/epoch/sequence and incarnation. `flue_conversation_stream_batches(path, seq, producer_*, data JSON, submission_id, attempt_id)` holds one row per **atomic batch** (`runtime/conversation-stream-store.ts:117-140`).
  - Append is all-or-nothing per batch (`:66-90`). It is fenced by producer epoch, so a stale owner's write fails (`:631-639`). It is idempotent on producer sequence: a retry with the same content is a no-op, and different content is an error (`:640-667`).
  - Submissions live in a separate queue table: status, attempts, lease, abort intent, settlement (`sql-agent-execution-store.ts:846-870`).
  - Optional fold checkpoints are a cache over the log, never the source of truth (`conversation-stream-store.ts:44-60`).
- **How it is watched:**
  - (a) `observe(subscriber)`: global, in-process and synchronous. It is not awaited, does not replay, and does not cross processes (`runtime/events.ts:21-52`, `:68-83`). OpenTelemetry is built on these observations (`packages/opentelemetry/src/index.ts:132` `invoke_agent`, `:214` `chat`, `:277` `execute_tool`; `instrumentation.ts:8-13`).
  - (b) HTTP `GET /:id?view=updates&offset=…&live=sse|long-poll`. It projects durable batches into `ConversationStreamChunk`s (`message-started`, `message-delta`, `tool-input`, `tool-output`, `tool-output-error`, `message-completed`, `submission-settled`, `conversation-reset`). Each chunk has a `position {batch, index}` to dedupe on, and the client resumes from an opaque offset (`apps/docs/src/content/docs/reference/streaming-protocol.md:21-34`, `:199-326`). In-process readers poll every 250 ms (`runtime/conversation-observer.ts:36`).
  - Deltas arrive in the live stream **from the durable log**, so a watcher who reconnects sees the same text.
- **What is rebuilt:**
  - Model context is computed by folding the records into a tree and walking from the active leaf. Compaction is applied along the way (`conversation-reducer.ts:1084-1139`).
  - Resume classifies what the durable evidence proves (`guide/durability.md:21-41`):
    - input never persisted: requeue;
    - completed response: settle;
    - partial text: tell the model it was interrupted and continue;
    - unresolved tool calls: an "unknown outcome" error result, no re-run;
    - `durable: true` tools: re-run, with `step.do` memos replayed (`:65-96`);
    - tasks: resume the child stream (`:98-107`).
  - Rule: "at-least-once execution over exactly-once recording" (`:39`).
  - Not persisted: `turn_request` (the full prompt), `toolcall_delta` (`types.ts:1233-1242`), sandbox files (`durability.md:144-150`).

### DeepSeek Harness (DSH)

The repo was verified at `deepseek-ai/deepseek-harness`, `22ddee37`. The Medium write-up returned 403 and was not read. The claim it quoted, "anything visible to the model must be reconstructable from the log", is **confirmed in primary source**: "**Model-visible means logged.** Every model request must be reconstructable from the log. New model-visible inputs require session events." (`docs/architecture.md`, "Session log" section, line 128).

- **Who writes:** the agent loop, through `Session.append`. Three event domains (`docs/architecture.md`, "Events"):
  - durable session events, broadcast via `session/event`;
  - live `agent/*` events;
  - capability waterfalls (`tools/*`, `llm/stream`).
- **When, relative to side effects:** the order is `turn/start` → `step/start` → `request/header` → `system/message` / `user/message` → model stream → `assistant/message` or `assistant/attempt` → `tool/call` → execute → `tool/result` → `step/end` → `turn/end` ("Turn flow"). The checkpoint plugin makes three barriers durable (`packages/session/session-checkpoint-policy/README.md:12`, `:48`, `:66`):
  - the request is flushed before the model stream is built;
  - a recorded `tool/call` is flushed **before the tool body runs**;
  - the previous step's response and results are flushed before the next request.

  These barriers fail closed (`:52`). Streaming chunks are *not* durable until settlement (`:115`).
- **Where:** a pluggable backend. The shipped one is a per-session append-only JSONL file, optionally zstd-framed, with torn-tail recovery and versioned format generations (`packages/session/session-persistence-jsonl/README.md:12`; `packages/session/session-format-v3-to-v4`, …). Event data must be plain JSON, validated at append (`types.ts:375-380`).
- **How it is watched:** `session/event` listeners for durable events (`packages/core/session/src/index.ts:70-76`). Process-local `agent/assistant-stream` start/chunk/end frames for live text. The web follow adapter is their only remote consumer ("Turn flow" paragraph). Telemetry and OTel are derived from the log (`packages/session/session-telemetry-otel`).
- **What is rebuilt:**
  - `deriveMessages()` projects model history from the log, and "Fork, resume, transcripts, telemetry, and persistence all derive from these durable settlements" (`docs/architecture.md` "Session log").
  - `assistant/message` embeds the exact timed, compacted provider stream (`packages/llm/llm/src/assistant-stream.ts:20-44`), so a UI can replay its typing, but only after the step settles.
  - Tail repair supplies the missing tool results: `TOOL_NOT_STARTED` when the call was never recorded, `TOOL_OUTCOME_UNKNOWN` when the call was recorded but no result was (`packages/core/session/src/repair.ts:14-18`). These carry model-facing advice: retry only idempotent work (`:30-40`).
  - The surface supports `replace` ops for compaction, and each event cites its sources (`types.ts:441-490`).

### Rig on keelrs's path

- keelrs calls `AgentRun::next_step` / `model_response` / `answer_all` itself, plus `DynModel::call` (`keelrs/src/agent/rig/mod.rs:72-171`). The `run` module emits **no spans and no events**: only `tracing::warn!` and `debug!` for odd cases (`rig-agent/src/run/patch.rs:52`, `run/prepare.rs:230`, `run/streamed.rs:241`, `:299`).
- **rig-core emits one `chat` span per model call** (`rig-core/src/operation/completion.rs:66-73`). It has gen_ai fields for provider, model, response id, provider request id, usage (input, output, cache, reasoning tokens) and optional input/output messages (`rig-core/src/telemetry/mod.rs:32-63`). It is parented to whatever span is current. Content is recorded only when `record_telemetry_content` is set; the default is `false` (`rig-core/src/completion/request.rs:1239`, `:1497`).
- **AgentRunner only**, which keelrs does not use:
  - `invoke_agent` root span (`rig-agent/src/agent/telemetry.rs:29-57`);
  - per-turn `chat` parent span (`:7-22`, `engine.rs:327`);
  - `execute_tool` spans carrying tool name, call id, arguments, result, outcome and error type (`telemetry.rs:59-73`, `engine.rs:509`, `:555`);
  - `info!` turn counters (`engine.rs:277`, `:430`).

  keelrs must create the equivalents itself.
- **Sans-I/O helpers keelrs can use for events:**
  - `AgentRunStep::CallModel {prompt, history, turn}`, `CallTools {calls}`, `Done` (`run/mod.rs:76-100`);
  - `ModelTurn {head, response_id, provider_request_id, choice, usage, policy, finish_reason, raw}` (`:103-121`);
  - `run.answer(...)` one call at a time, or `answer_all` (`:1360`, `:1373`);
  - `project(&run.messages()[cursor..])` from `projection_start()`, which yields each committed `ToolCall` / `ToolResult` after every protocol call (`run/committed.rs:53-90`, `mod.rs:639`);
  - `StreamedTurnAssembler` events (`EmitIngested`, `EmitToolCallDelta`, `EmitToolCall {call}`) for live text and tool-call streaming, if keelrs moves to streaming (`run/streamed.rs:171-191`).
- `AgentRun` is serde-serializable with `RUN_FORMAT = 5` and refuses any other format (`run/mod.rs:304-372`). That is why ADR 0004 never stores it. `next_step` is idempotent and re-issues `CallModel` for the same attempt after a restart (`:829-846`).

### keelrs today: the gaps at each point a trace, subscriber or resume needs

| Point | What exists | Gap |
|---|---|---|
| dispatch admitted | `Command::Dispatch` in memory (`instance.rs:62-72`) | No event. `SubmissionId` is a process-global `u64` counter (`:26`), not durable-unique. |
| model turn start / request | none | No turn number, model or tools recorded. No span of its own; rig-core's `chat` span has no keelrs parent. |
| model turn text | in the final `Settled.messages` only (`rig/mod.rs:160-168`) | Nothing until the dispatch settles. No usage or finish reason kept. |
| tool call + args | inside the assistant `Message` only | No event before execution, which is what write-ahead needs. No call ordinal or timestamp. |
| tool result / error | inside the user `Message` only; errors are `ToolResult::failed` text (`rig/mod.rs:202-207`) | No duration, and no way to tell error from model-refusal from timeout. A timeout is just error text (`:195-199`). |
| run failed / aborted | `Settled {Err(Outcome)}` (`instance.rs:167-173`) | **Every message and tool call of the failed run is dropped** (`:162-168`), including tool side effects that already happened. |
| history for the next dispatch | `Vec::new()` (`instance.rs:156`) | Not yet built from events. |
| storage | `HashMap` of settled results (`:130`, `:175`) | No store and no sequence. |
| subscription | none | — |

## 3. Ideas keelrs should take

1. **Two layers, with one rule between them.** Keep durable canonical events: an append-only log per agent instance, the only input to history and resume. Keep live observations separately: ephemeral, in-process, `tokio::sync::broadcast`. A live observation fires only after its durable event is stored (Flue `session.ts:2374-2390`, `:2641-2722`). Text and tool-argument deltas stay live-only at first, as in DSH and in Flue's `toolcall_delta`.
2. **An envelope that serves flows unchanged:**
   - `instance_id`, a per-instance `seq` (contiguous, assigned by the store under one writer), `event_id` (deterministic for idempotent events such as `tool_result:<call_id>`), `at` (UTC), and `format` version;
   - causation: `submission_id` (durable: a UUID/ULID, not an `AtomicU64`), `attempt`, `turn` (the AgentRun model-call index, summed across rebuilds), `tool_call_id`;
   - for flows later: optional `parent: {instance_id, tool_call_id | step}` and a `run_id` / correlation id, as in Flue `child_session_retained` / `conversation_created{kind: task}` and fluex `<runId>-<step>`.
3. **A small catalog that matches the AgentRun steps.** A user can see everything except streaming text and argument deltas.

   | Event | Fields | Written when |
   |---|---|---|
   | `SubmissionAccepted` | text, idempotency key | at `dispatch`, before the receipt returns, so the receipt means accepted (Flue `durability.md:11-13`) |
   | `TurnStarted` | turn, model id, thinking, tool names (or a hash of the declaration) | before `model.call` |
   | `AssistantTurn` | the Rig assistant message as AgentRun accepted it, usage, finish reason, response id, provider request id | after `model_response` |
   | `ToolCallStarted` | call id, name, args | written and **flushed before the tool body runs** |
   | `ToolCallFinished` | call id, outcome `ok \| error \| refused \| timeout \| malformed`, result content, duration | — |
   | `Settled` | outcome, error, totals | — |

   With these, `AssistantTurn` + `ToolCallFinished` + `SubmissionAccepted` rebuild the Rig `Message` history for a new `AgentRun`. Use `project()` from `projection_start()` to pull each committed call and result out of the run after `answer`.
4. **Resume rules from Flue and DSH.**
   - A recorded call with no result becomes a synthetic error result, "outcome unknown, retry only if idempotent". The tool is never re-run (DSH `repair.ts:14-40`, Flue `durability.md:37`).
   - A call that was never recorded was never run.
   - An accepted submission always reaches exactly one `Settled`.
   - Also persist the messages of failed and aborted runs. Drop today's discard in `instance.rs:162-168`.
5. **Store shape (sqlx).**
   - One `events(instance_id, seq, submission_id, kind, data jsonb, at, PRIMARY KEY(instance_id, seq))` table, written in **atomic batches**: a tool batch's results together, as in Flue's batch contract.
   - A submissions table holding status and attempts.
   - A reader that resumes from an opaque `seq`/offset with "upToDate", and dedupes by `(seq, index)`, gives observers on other processes the Flue `view=updates` behaviour.
6. **`ignorable` / refuse-unknown on read** (DSH `types.ts:509-520`). An event kind the reader does not know, and that is not marked ignorable, aborts the load instead of rebuilding wrong history. Add a `format` version per event.
7. **Tracing from keelrs, not Rig.** Open an `invoke_agent`-style span per dispatch, with instance and submission ids, and enter it around `model.call`; rig-core's `chat` span then nests under it. Add an `execute_tool` span per call, using the same gen_ai field names as `rig-agent/src/agent/telemetry.rs:59-73`. Flue and DSH both derive OTel from their own events; keelrs can do the same from its observation stream.
8. **Operator fields from fluex.** Call ordinal `n`. `failed` kept apart from `is_error`. Output byte count plus a clipped tail for the live view, while the durable event keeps the full content. A settle summary of turns, tool calls, errors, tokens and wall time.

## 4. Ideas keelrs should avoid

- **Per-delta durable records with timer coalescing** (Flue `assistant_text_delta` with a 1 s flush, `conversation-writer.ts:34-41`, `:151-187`). This is a lot of machinery and write volume. DSH's choice, a live-only stream with the full message durable at settle, is simpler. keelrs calls `model.call`, not streaming, so it has no deltas yet anyway.
- **Storing Rig types in the log as-is.** ADR 0004 already rules out `AgentRun` JSON. Note that `rig_core::Message` serde is also Rig's format: store a keelrs-owned shape, or version it.
- **A global, synchronous, non-replaying observer as the only live surface** (Flue `observe()`, `runtime/events.ts:21-52`). fluex had to bolt on files to follow a run from another process (`run-cli.md:41-46`). Pair the in-process broadcast with a read-from-`seq` API on the store.
- **A lossy side log as the operator view** (fluex `events.jsonl`, clipped at 2000 chars). Make the canonical log complete and clip only in presenters.
- **Unpersisted prompts.** Flue never stores `turn_request` (`observability.md:13`). DSH logs `request/header` and `system/message` because "model-visible means logged". keelrs re-renders the agent function each turn, so record what the render declared (instructions, tool set, model) per `TurnStarted`, or a hash of it. Otherwise resume cannot prove what the model saw.
- **Over-building before flows exist.** Leave out Flue's producer epochs, leases, fold checkpoints and incarnation ids, and DSH's surface `replace` ops and format-migration chain, until multi-process ownership or compaction is actually needed. Keep the envelope fields that make them possible: `seq`, `event_id`, `format`, `attempt`.

## Sources

- fluex: `packages/flue-tools/src/tool-log.ts`, `mount-tool-log.ts`, `interceptors.ts:58-71`; `packages/server/src/flows/runs.ts`, `run-types.ts`, `ask.ts`; `packages/sdk/src/wire.ts`, `run-id.ts`; `packages/cli/src/agent-client.ts`, `line-presenter.ts`; `docs/research/observability.md`, `run-cli.md`, `flows.md`.
- Flue (`packages/runtime/src/`): `conversation-records.ts`, `types.ts:1150-1430`, `observation.ts`, `runtime/events.ts`, `conversation-writer.ts`, `runtime/conversation-stream-store.ts`, `session.ts` (lines cited above), `conversation-reducer.ts`, `runtime/conversation-observer.ts`, `sql-agent-execution-store.ts`, `instrumentation.ts`; `packages/opentelemetry/src/index.ts`. Docs `apps/docs/src/content/docs/`: `guide/durability.md`, `reference/streaming-protocol.md`.
- Rig 0.46.0-rc.1: `rig-agent/src/run/{mod,committed,streamed,patch,prepare}.rs`, `rig-agent/src/agent/{telemetry,engine}.rs`, `rig-core/src/operation/completion.rs`, `rig-core/src/telemetry/mod.rs`, `rig-core/src/completion/request.rs`.
- DeepSeek Harness: [github.com/deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) `docs/architecture.md`, `packages/core/session/src/{types,known-event-types,repair,index}.ts`, `packages/llm/llm/src/assistant-stream.ts`, `packages/session/session-checkpoint-policy/README.md`, `packages/session/session-persistence-jsonl/README.md`. Secondary sources, not relied on for facts: [levelup.gitconnected.com write-up](https://levelup.gitconnected.com/inside-deepseeks-170k-stars-open-source-agent-harness-837455df53d0) (403, not read); [thenewstack.io](https://thenewstack.io/deepseek-harness-open-source-plugins/) and [digitalapplied.com](https://www.digitalapplied.com/blog/deepseek-harness-open-source-agent-framework-2026), seen only as search snippets. They disagree on star counts; the GitHub API reported 263k on 2026-10-27. **Unverified:** their claims about Cordis internals and the release date.
- keelrs: `docs/adr/0004-keelrs-drives-rigs-agentrun.md`, `keelrs/src/event.rs`, `keelrs/src/agent/instance.rs`, `keelrs/src/agent/rig/mod.rs`.
