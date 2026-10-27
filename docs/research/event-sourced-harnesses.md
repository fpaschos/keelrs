# Event-sourced agent harnesses and durable execution: how runs are recorded, replayed and exposed

Research for keelrs (a Rust agent framework built on Rig). In keelrs, canonical events are the source of truth, the canonical log is also the durability journal, and each agent instance is a single-writer tokio actor.

Researched 2026-10-27. Each claim cites its source in brackets, keyed to the Sources list at the end. Claims marked **[unverified]** come from secondary write-ups. Claims marked **[inference]** are my reading, not a statement any source makes.

---

## 1. iterate: the "event-sourced agent harness" workshop and its production engine

The workshop [S1, S2] is the teaching version. The `iterate/iterate` monorepo [S3] holds the production engine that grew out of it: `core/lib/src/stream/processor.ts` and `core/lib/src/agents/processor.ts` at commit `13b90be`, 2026-10-02. Workshop names map to current names: `afterAppend` is now `processEvent`, and `reduce` keeps its name. I could not find `afterAppend` anywhere on `main` today.

**What it records.**
- In the workshop, events are JSON with `type` and an optional `payload`. The server assigns a monotonically increasing `offset` and `createdAt`. An `idempotencyKey` prevents a double append. Streams are addressed by a slash path [S2].
- The current envelope (`StreamEventInput` / `StreamEvent`) [S3 processor.ts] has these fields:
  - `type`, written as a namespaced string such as `events.iterate.com/agent/llm-request-settled`.
  - `payload` and `metadata`.
  - `idempotencyKey`. The rule is "Same key + same body = dedupe (the existing event is returned); different body = loud error" (`IDEMPOTENCY_CONFLICT`).
  - An optional `offset` precondition on append: "land at exactly this offset or refuse the whole batch with OFFSET_CONFLICT". This is optimistic concurrency.
  - `ephemeral: true`. An ephemeral event "rides the stream to live subscribers but is NEVER persisted: it consumes an offset, triggers zero writes".
  - Fields the platform stamps at commit: `offset`, `createdAt`, `path`, and `source`. `source` holds `origin`, `cause {chain, depth, parent: "<path>@<offset>"}`, `processor {slug, version, whileProcessing {offset, type}}`, `principal`, and `onBehalfOf`. The cause depth is capped at 8 hops to stop reaction loops (`itx/loop-limit`).
- Agent event catalog [S3 agents/processor.ts]:
  - Lifecycle: `create-requested`, `created`, `create-failed`, `delete-requested`, `deleted`, `configured`, `paused`, `resumed`.
  - Conversation: `context-added`, which carries the role (user, assistant, developer).
  - Model calls: `llm-request-requested` (the intent), `llm-request-settled` (`result.status` is `succeeded`, `failed` or `cancelled`, and a cancellation carries `reason` plus `partialText`), and `token-usage-reported`.
  - Streaming: `llm-response-frame`, which is **ephemeral**.
  - Scripts: `itx/run-requested`, which is how code execution is requested.

**Who writes, and when relative to the side effect.**
- Processors are pure classes with three hooks [S3 processor.ts]:
  - `reduce`, a pure fold that returns new state.
  - `processEvent`, a synchronous effect hook that registers async work through `blockProcessorWhile` or `runInBackground`.
  - `projectLiveState`.
- Effects only append new events. Nothing writes state directly.
- The model call follows a write-before / write-after pair [S3 agents/processor.ts]:
  1. At head, the processor appends `llm-request-requested` with `idempotencyKey = <slug>/request/<triggerOffset>`. The body is deterministic: "expiresAt anchored at the trigger's time, never `now`".
  2. The reduce turns that intent into `openRequest`. The request's identity is the offset of the intent event.
  3. Only then does `processEvent` see an open request that "nobody HERE is running" and start the call.
  4. Completion appends `llm-request-settled` with key `settle/<requestOffset>`. It goes in **one batch** with the assistant `context-added` and the usage, "so an eviction between them is impossible".
- An interruption settles the request as `cancelled` and appends the partial text as context. "a success that raced it loses on the settle key."
- Two kinds of effect [S3 agents/processor.ts header]:
  - A per-event consequence is **blocked**. The cursor does not advance until its append lands, "the event is delivered once, so losing the append would lose the consequence".
  - A state-derived consequence runs in the background at head. "any later delivery over the same fold re-derives it ... every append is idempotency-keyed so a retry appends nothing twice."

**Checkpoint and commit rule.** "ONE DURABLE COMMIT PER BATCH, after every event's blocking work settled — persist BEFORE advancing past the last DURABLE offset. A failed batch persists nothing, retried whole" [S3 processor.ts]. The checkpoint is one row per processor slug: `(reducerVersion, reducedThroughOffset, stateJSON)`. Writing one row means cursor and state can never disagree.

**Crash and resume.**
- Catch-up reads the log from the checkpoint offset in pages of 500.
- A gap in pushed deliveries triggers "gap repair" from the processor's own cursor.
- Background work holds an alarm claim ("come back by T"). A dead host is revived, catches up and runs the "at-head pass". This is how an open LLM request with no runner is restarted or settled as expired.
- Deaths are counted, and after 5 (`MAX_DEATHS`) a revive no longer restarts the work.

**Replay without side effects.** Bumping `contract.version` "re-reduces from offset 0 through `reduce` only, never re-running side effects — over durable rows only, which is why durable product truth must never derive from an ephemeral" [S3 processor.ts]. Normal catch-up of events not yet processed does run effects. That is at-least-once, so idempotency keys are mandatory.

**Live observers.**
- In the workshop: `?live=true` returns SSE, and the pattern is "first read the full stream history and then switch to `live: true`" [S2].
- In the current engine: live state is a projection with a revision chain. Each change emits an ephemeral `itx/live-state-changed` delta, and a processor may never reduce over it ("feedback-loop class, made unspellable") [S3].
- Streaming tokens go out as ephemeral `llm-response-frame` windows. "a lost window loses only its repaint; the settlement is the truth."

## 2. Durable Streams (protocol under the workshop server)

Source: [S4] README and [S5] PROTOCOL.md.

- **Records.** Each stream is an append-only byte or JSON sequence over HTTP. In JSON mode each POST is stored as distinct messages, and one level of array is flattened.
- **Offsets.**
  - Offsets are "opaque, case-sensitive strings", lexicographically sortable, strictly increasing, and unique per position.
  - Two sentinels: `-1` means the beginning and `now` means the tail. A server must never generate them as real offsets.
- **Writer idempotency.**
  - Headers `Producer-Id`, `Producer-Epoch` and `Producer-Seq` are sent together.
  - Same epoch with `seq <= last` returns 204 (duplicate). `seq = last+1` is accepted. A gap returns 409 with `Producer-Expected-Seq`.
  - A lower epoch returns 403. This is **fencing** of a stale writer.
  - Servers MUST serialize per `(stream, producerId)`.
  - There is also an optional `Stream-Seq` monotonic check.
- **Readers.**
  - Catch-up: `GET ?offset=`.
  - Live: `live=long-poll` or `live=sse`. SSE sends a `control` event after data, carrying `streamNextOffset`, `upToDate` and `streamClosed`.
  - `Stream-Next-Offset` is the resume token. `Stream-Up-To-Date: true` marks the tail and does not mean EOF.
  - Closure is durable and monotonic (`Stream-Closed: true`).
  - The server SHOULD close SSE about every 60 s, and the client reconnects from the last offset.
- **Takeaway for keelrs.** "Caught up" (up-to-date) is a different signal from "closed" (EOF). Producer epoch gives a clean way to fence a zombie writer after an actor is restarted.

## 3. PLDI'26 / PAgE: "Event-based Design Abstractions for Agent Harnesses"

- **Authors.** McCoy Becker, Matin Ghavami, Fabian Zaiser, Timothy J. O'Donnell, Martin C. Rinard, Joshua B. Tenenbaum and Vikash Mansinghka (MIT, McGill/Mila). Presented at PAgE 2026, co-located with PLDI, 15 June 2026 [S6].
- **Abstract.**
  - A durable append-only event log is the single source of truth for harness state.
  - Harness concerns are "facets" that (1) **fold**, deriving local state by pure folds over the log, (2) **admit**, authorizing new events through an admission rule, and (3) **propose**, offering follow-on events when their state changes.
  - The log is the coordination medium between facets, and an external trigger drives rounds of admission and proposal steps.
  - The authors mention a "concise Python implementation" [S6].
- **Paper and code.** Neither is linked from the program page, and I found no arXiv or repo. Treat everything beyond the abstract as unknown.
- **Mapping to keelrs [inference].**
  - fold = `reduce`.
  - propose = the effect hook emitting candidate events.
  - admit = a validation gate in the actor before an append commits, such as budget, permission, or "a request is already open". This is the slot where policy (tool approval, turn and budget limits) belongs, kept apart from effects.
  - iterate's reducer does a weak form of admission by ignoring "late intents" that do not match the pending trigger [S3].

## 4. Temporal

- **What it records.**
  - The Event History is "an append-only log of Events", durably persisted by the Temporal Service [S7].
  - The worker sends **Commands**, and the Service writes **Events** [S7].
  - For an activity, `ActivityTaskScheduled` is written when the workflow commands it. While the activity runs and retries, "ActivityTaskScheduled is the only Activity-related Event in History". `ActivityTaskStarted` is written together with the terminal `ActivityTaskCompleted` or `ActivityTaskFailed` [S7].
  - So it is **write-before (scheduled) + write-after (result)**, and individual attempts are not recorded in history.
- **Determinism and replay.**
  - Workflow code re-executes on replay, and the Commands it emits "are compared with the existing Event History". A mismatch is a non-determinism error [S8].
  - Time and randomness must come from SDK APIs, whose results are stored in history [S8].
  - LLM calls and I/O go in Activities, which "execute outside the replay path and are automatically retried" [S8].
  - So the LLM response is recorded once, as the activity result, and replayed from history.
- **Idempotency.**
  - Activities are at-least-once: they "may be executed multiple times and may even partially complete more than once".
  - The recommended key is Workflow Run ID + Activity ID, stable across retries [S9].
  - Long activities must heartbeat [S9].
- **Large payloads.**
  - The payload limit is 2 MB per payload, configurable when self-hosted. gRPC requests are capped at 4 MB.
  - The remedy is the claim-check pattern: "Store large data in an object store, pass references". The SDKs build it in as External Storage or a custom Payload Codec [S10].
  - History length: a warning at 10,240 events and termination past 51,200 events. Continue-As-New avoids the limit [S7].
- **Live observers.** These come through the Service's history and visibility APIs. Agent token streaming is experimental in the OpenAI integration (next item).
- **OpenAI Agents SDK integration.** The package moved to `temporalio-openai-agents`, with the API in `temporalio.openai_agents` [S11, S12].
  - "model invocations are automatically routed through Temporal activities".
  - `activity_as_tool()` turns an activity into a FunctionTool.
  - A plain `@function_tool` runs inside the workflow and must be "deterministic, workflow-safe logic".
  - Every MCP operation is an activity.
  - `WorkflowSession` items "live in workflow state and are reconstructed on replay".
  - Streaming (`run_streamed`) is experimental and needs a stream topic.
  - Tracing: "Spans are only exported when workflows actually complete, not during replays", with "Consistent span IDs across workflow replays" [S12].
  - So telemetry is suppressed on replay and its IDs are deterministic. keelrs needs the same property if it emits spans while folding a log.

## 5. Restate

- **What it records.** A per-invocation **journal**. For side effects, "Restate records both the operation and its result". The SDK sends journal entries to the Restate Server, and "If the service cannot send journal entries to the Restate Server, it will stop the handler execution" [S13].
- **Durable steps.**
  - `ctx.run(name, fn)` stores the result "in the execution log" and is used "to replay operations after failures" [S14].
  - Failures are retried with a configurable policy (`maxRetryAttempts`, `maxRetryDuration`, and so on). `TerminalError` stops retries [S14].
  - Context calls cannot be nested inside `run`.
  - `ctx.rand` is seeded by the invocation ID, and `ctx.date.now()` is stable across retries [S14].
  - On crash: "Restate replays the journal, skipping completed steps and resuming from exactly where it left off" [S15].
  - **[inference]** A `run` whose result was not journaled before the crash re-executes, so the body is at-least-once. The docs do not spell out exactly when the write happens [S14].
- **Agent integration rules** [S16]:
  - Wrap every LLM call in `ctx.run()` and cap retries "to prevent ... high LLM costs".
  - Wrap tools, including built-in tools such as web search, in `ctx.run`.
  - Persist SDK-generated IDs and timestamps.
  - **Disable native parallel tool execution**, because "the order of completion may differ on retries, leading to a non-deterministic journal".
  - Streaming is not supported inside `ctx.run`.
  - Never let the suspension error reach the LLM.
- **Idempotency.** Invocations accept an idempotency key, and "Duplicate requests will return the same result" [S15]. Streaming updates to a UI go through a pubsub virtual object, keyed with `ctx.rand.uuidv4()`, because "Without this idempotency key, each replay/retry leads to duplicate events" [S17].
- **Live observers.**
  - SSE from the pubsub object, read from an offset [S17].
  - The UI shows the invocation list, a journal view (inputs, outputs, timings) and a state view [S18].
  - Restate exports OpenTelemetry traces, and "Each durable step appears as a span" [S18].

## 6. LangGraph checkpointers

- **What it records.** A **state snapshot per super-step**, not an event log: "LangGraph creates a checkpoint at each super-step boundary" [S19].
  - `StateSnapshot` holds `values`, `next`, `config {thread_id, checkpoint_ns, checkpoint_id}`, `metadata {source: input|loop|update, writes, step}`, `created_at`, `parent_config` and `tasks` (including interrupts) [S19].
  - Saver interface: `put`, `put_writes`, `get_tuple` and `list`.
- **Pending writes.**
  - Each node's writes are stored as it finishes (`put_writes`). The snapshot commits when the super-step completes.
  - If one node fails, the saver keeps "pending checkpoint writes from any other nodes that completed successfully", and on resume "you don't re-run the successful nodes" [S19].
- **Durability modes** [S19]:
  - `exit`: persist only when the graph exits.
  - `async`: persist while the next step runs, with a small risk of a missed checkpoint.
  - `sync`: persist before the next step starts.
- **Replay and fork.**
  - Invoking with an old `checkpoint_id` skips nodes before it and **re-executes** nodes after it, "including LLM calls, API requests, and interrupts" [S19].
  - `update_state` creates a new checkpoint (a fork) and never mutates the old one [S19].
  - So there is no recorded-result replay of LLM calls past the checkpoint.
- **Interrupted tool calls and interrupts.**
  - On resume "the runtime restarts the entire node from the beginning", and "side effects called before interrupt should (ideally) be idempotent" [S20].
  - `interrupt()` calls are matched by position, so their order must be stable.
  - `interrupt()` can live inside a tool for approval [S20].
  - **[inference]** A crash in the middle of a tools node re-runs the whole node, so every tool call in it runs again unless it completed as its own pending write.
- **Lesson for keelrs.** Granularity matters. A snapshot per step loses everything inside the step. An event per tool call start and finish lets keelrs re-run only the unfinished calls.

## 7. OpenTelemetry GenAI semantic conventions

Status: **Development**. The conventions have moved to `open-telemetry/semantic-conventions-genai`, and the opentelemetry.io pages say "This page has moved" [S21]. I read the repo at commit `e19717d`, 2026-10-27 [S22].

**Spans and kinds** [S22: `docs/gen-ai/gen-ai-agent-spans.md`, `gen-ai-spans.md`, `client-inference.md`]:

| Operation | Span name | Kind |
| --- | --- | --- |
| `create_agent` | `create_agent {gen_ai.agent.name}` | CLIENT |
| `invoke_agent` (remote agent) | `invoke_agent {gen_ai.agent.name}`, or plain `invoke_agent` | CLIENT |
| `invoke_agent` (in-process agent) | same as above | **INTERNAL** (keelrs' case) |
| `invoke_workflow`, plan, skill spans | defined in the agent spans doc | (not checked) |
| Inference (`chat`, `generate_content`, `text_completion`) | `{gen_ai.operation.name} {gen_ai.request.model}` | CLIENT, or INTERNAL for an in-process model |
| `execute_tool` | `execute_tool {gen_ai.tool.name}` | INTERNAL |

**Attributes.**
- Inference, Required: `gen_ai.operation.name` and `gen_ai.provider.name`.
- Inference, Conditionally Required: `error.type`, `gen_ai.conversation.id`, `gen_ai.request.model`, `gen_ai.request.stream`, `gen_ai.output.type`.
- Inference, Recommended: `gen_ai.response.id`, `gen_ai.response.model`, `gen_ai.response.finish_reasons`, `gen_ai.response.time_to_first_chunk`, `gen_ai.usage.input_tokens` and `output_tokens`, cache read and write tokens, and request parameters.
- `invoke_agent` adds `gen_ai.agent.name`, `gen_ai.agent.id`, `gen_ai.agent.description`, `gen_ai.conversation.id` and `gen_ai.data_source.id`.
- `execute_tool`:
  - Required: `gen_ai.tool.name`.
  - Conditionally Required: `gen_ai.tool.call.id`, `gen_ai.agent.name`, `gen_ai.conversation.id`, `error.type`.
  - Recommended: `gen_ai.tool.type` and `gen_ai.tool.description`.

**Content capture.**
- "SHOULD NOT capture them by default, but SHOULD provide an option for users to opt in."
- These attributes are Opt-In: `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions`, `gen_ai.tool.call.arguments` and `gen_ai.tool.call.result`.
- `gen_ai.tool.definitions` is Opt-In but not governed by the content flag.
- The environment variable `OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT` takes `NO_CONTENT` (the default), `SPAN_ONLY`, `EVENT_ONLY` or `SPAN_AND_EVENT`.
- The production recommendation is to store content externally and record references on spans. Upload goes through a hook (`OTEL_INSTRUMENTATION_GENAI_CONTENT_HOOK`, `..._UPLOAD_BASE_PATH`, `..._UPLOAD_FORMAT` json or jsonl).
- "Streaming chunks" is still TODO [S22 gen-ai-spans.md].

**Events.**
- `gen_ai.client.inference.operation.details` carries inputs and outputs [S22 client-inference.md].
- `gen_ai.evaluation.result` is parented to the evaluated span [S22 gen-ai-events.md].
- The message JSON schemas (`gen-ai-input-messages.json` and `gen-ai-output-messages.json`) use roles with typed `parts` (`text`, `tool_call {id,name,arguments}`, `tool_call_response {id,response}`).

**The Rust angle** (`tracing-opentelemetry` 0.34, built on `opentelemetry` 0.33) [S23]:
- Special span fields: `otel.name` gives a dynamic name such as `"execute_tool web_search"`. `otel.kind` takes `"client"` or `"internal"`, and spans default to internal. `otel.status_code` and `otel.status_description` set the status.
- Other fields such as `gen_ai.operation.name = "chat"` pass through as attributes.
- `OpenTelemetrySpanExt` bridges to OTel `Context`, for example to set a parent.
- **[inference]** Dotted names need quoting in macros, for example `"gen_ai.usage.input_tokens" = n`. Fields recorded later must be declared up front as `tracing::field::Empty`. That matters because usage and finish reasons arrive only at settle.

## 8. OpenAI Agents SDK tracing

Source: [S24, S25].

- **Model.**
  - A trace holds `workflow_name`, `trace_id` (`trace_<32 alnum>`), `group_id` (for example a conversation), `disabled` and `metadata`.
  - Spans hold `trace_id`, `parent_id`, `started_at`, `ended_at` and typed `span_data`. Types: agent, generation, function (tool), guardrail, handoff, custom, plus task, turn, transcription and speech.
  - The whole `Runner.run` is wrapped in a trace. "Tracing is enabled by default."
- **Sensitive data.** `trace_include_sensitive_data` **defaults to True**. Note that this is the opposite of the OTel default. It can also be set with `OPENAI_AGENTS_TRACE_INCLUDE_SENSITIVE_DATA`.
- **Processors.**
  - `TracingProcessor` has `on_trace_start`, `on_trace_end`, `on_span_start`, `on_span_end`, `shutdown` and `force_flush`.
  - The `on_*` hooks are "Called synchronously" and must "return quickly". `on_span_end` "Should not block or raise exceptions".
  - `BatchTraceProcessor` with an exporter exports in the background every few seconds.
  - `add_trace_processor()` adds a processor next to the default one. `set_trace_processors()` replaces the defaults, which is the right way to redact before export.
- **Relevance.** This is an in-memory span stream with fan-out to processors. Spans are not durable and are not the source of truth.

## 9. Claude Agent SDK telemetry and sessions

- **Telemetry** [S26]:
  - The SDK runs the Claude Code CLI as a child process. The SDK emits nothing itself, and the CLI exports OTLP.
  - Three independent signals: metrics, log events (prompt, API request or error, tool result, `tool_decision`, `safety_event`) and traces (beta; requires `CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1`).
  - Spans: `claude_code.interaction` (one turn) is the parent of `claude_code.llm_request`, `claude_code.tool` and `claude_code.hook`. `claude_code.tool` has children `claude_code.tool.blocked_on_user` (the permission wait) and `claude_code.tool.execution`.
  - Subagent spans nest under the parent's tool span.
  - The `session.id` attribute is on spans by default.
  - W3C context propagates in through `TRACEPARENT` and `TRACESTATE` and out to Bash commands.
  - Content is off by default. Opt-in: `OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_TOOL_DETAILS`, `OTEL_LOG_TOOL_CONTENT` (truncated at 60 KB) and `OTEL_LOG_RAW_API_BODIES` (inline, or `file:<dir>` with a `body_ref`). The last is a claim-check for telemetry.
  - Export is batched (traces and logs every 5 s), and data is lost if the process is killed. Telemetry is best-effort, not a journal.
  - It uses its own `claude_code.*` names, not GenAI semconv names.
- **Sessions** [S27]:
  - The durable record is the session transcript: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, holding the prompt, every tool call, every tool result and every response.
  - `resume` continues a session. `fork_session` copies the history into a new ID.
  - A `SessionStore` adapter mirrors transcripts for resume on another host.
  - Sessions persist the conversation, not the filesystem. File checkpointing is a separate mechanism.
  - So the transcript (the log) and the telemetry (OTel) are separate pipelines.

## 10. Perplexity SPACE (snapshots, not an event log)

- SPACE runs a Firecracker microVM per sandbox [S28, S29].
- Lifecycle [S28]:
  - Running.
  - Paused: memory stays resident, and this "is not a durable snapshot".
  - Suspended: "A snapshot of VM memory, runtime state, and root filesystem".
  - Deleted.
- Files are "persisted at lifecycle boundaries, not per write" [S28].
- Rolling disk snapshots (btrfs CoW) and less frequent full VM checkpoints let the filesystem rewind [S29].
- On suspend, artifacts upload to object storage, and a DB row marks the snapshot restorable only after every artifact has arrived. Any node can restore [S29].
- The control plane reconciles desired against observed state with idempotent operations [S29].
- Command output streams support `Last-Event-ID` reconnect "while the guest still retains it" [S28].
- No event log is described.
- Secondary reports say snapshots can run every minute and a session can rewind up to a week **[unverified]** [S30].
- **Lesson.** Environment state such as sandbox files can't be event-sourced cheaply. The log should record a *reference* to an environment snapshot (snapshot ID or digest) as an event, not the bytes.

---

## Comparison at a glance

| System | Unit recorded | Writer | Relative to the effect | Resume after a crash | Live view |
| --- | --- | --- | --- | --- | --- |
| iterate | domain events (intent + settled), ephemerals | processors, through `append`; platform stamps provenance | intent before, settle after; idempotency-keyed | catch-up from per-processor checkpoint; revive alarm re-runs open requests | SSE or push; ephemeral frames; live-state deltas |
| Durable Streams | opaque messages at offsets | any producer, fenced by epoch/seq | n/a (transport) | read from the last `Stream-Next-Offset` | long-poll or SSE with `upToDate` |
| Temporal | Commands become history events | Service writes; worker proposes | Scheduled before; Started+Completed after | re-execute workflow code against history; activities retried | history and visibility APIs |
| Restate | journal entries per invocation | SDK streams entries to the server | `run` result journaled after the body | replay journal, skip completed steps | UI journal view; pubsub SSE; OTel |
| LangGraph | state snapshot per super-step + pending writes | checkpointer (`put`, `put_writes`) | after each node or step (durability mode) | restart from last checkpoint; successful nodes not re-run; interrupted node restarts | `stream` modes; state history |
| OpenAI Agents SDK | in-memory traces and spans | SDK through processors | span start and end | none (not durable) | processors |
| Claude Agent SDK | JSONL transcript + OTel | CLI | transcript per message; OTel batched | `resume` from transcript | message stream; OTel |
| SPACE | VM and filesystem snapshots | worker and control plane | periodic or at lifecycle boundaries | restore snapshot on any node | command event stream |

---

## Rules for keelrs

**Log and write ordering**

1. **One log, two roles.** The per-instance canonical log is both the domain truth and the durability journal. The actor is its only writer. Everything else is derived and rebuildable from it: the reduced state, the context window, live views and telemetry.
2. **Write-before for every non-replayable effect** (LLM call, tool call, sub-agent spawn, sandbox exec). Append and fsync an *intent* event first, such as `ModelRequestStarted{request_id, model, input_ref}` or `ToolCallStarted{call_id, tool, args}`, and only then perform the effect. The intent's ID is its identity, as in iterate's "the request's identity is the offset of the intent" and Temporal's `ActivityTaskScheduled`.
3. **Write-after with the result in one atomic append.**
   - `ModelResponseCompleted`, the assistant message and `UsageReported` commit in **one transaction** (iterate's single settle batch).
   - `ToolCallCompleted{call_id, result | error}` likewise.
   - On replay the recorded result is used and the effect is never re-run (Temporal, Restate).
4. **Resume rule for an open intent, meaning an intent with no terminal event.**
   - LLM calls are re-runnable. Re-issue them, or settle them `Cancelled{reason: expired}` past a deadline.
   - For tools, decide **per tool**: a tool marked idempotent is re-executed with the same `call_id` as idempotency key, and any other tool is settled `Interrupted` and the model sees a tool error.
   - Never silently drop an unmatched tool call. Providers reject an orphan `tool_use`, and LangGraph's whole-node restart is the failure mode to avoid.
   - Re-run only the unfinished calls of a parallel batch.
5. **Atomic checkpoint.** If keelrs persists reduced snapshots, store `(reducer_version, through_seq, state)` as one row in the same transaction as, or after, the events. A version bump re-folds from 0 through `reduce` only, with no effects (iterate).

**Idempotency**

6. **Idempotency on every append.**
   - The envelope carries an optional `idempotency_key`, unique per `(instance_id, key)`.
   - Same key + same body returns the existing event. Same key + a different body is a hard error (iterate).
   - Derive keys deterministically from the cause, such as `"{effect}/{intent_seq}"`, never from `now()` or random values.
   - Pass the intent's ID to external APIs as their idempotency key (Temporal's run ID + activity ID).
7. **Optimistic concurrency plus fencing.**
   - The store's append takes `expected_seq`, implemented as a unique `(instance_id, seq)` constraint, to catch a second writer.
   - Add a writer epoch or lease (Durable Streams `Producer-Epoch`) so a zombie actor after a restart is rejected.

**Determinism**

8. **Make inputs deterministic.** Anything the fold or a prompt depends on (timestamps, IDs, random values) is generated once and recorded in an event (Restate `ctx.rand` and `ctx.date`, Temporal SDK time). Reducers are pure and never read clocks.
9. **Serialize completion order.** For parallel tool calls, the log records completion order as it happened, and reduce must be order-independent per `call_id` (Restate disables native parallelism for exactly this reason).

**Canonical vs ephemeral**

10. **Two classes of event, and they never mix in folds.**
    - Canonical (persisted, sequenced): lifecycle, user input, model request and response, tool call and result, approval, interrupt, cancel, error, usage, environment snapshot refs.
    - Ephemeral (broadcast only, never persisted): token deltas, progress ticks, live-state diffs.
    - Durable truth must never derive from an ephemeral (iterate). The completed response event carries the full text, and lost deltas are only "repaint".
11. **Admission is a separate step** (PLDI "admit"). Before an effect's intent is appended, the actor runs admission rules: budget, turn limit, tool permission and human approval. Denials are themselves canonical events (Claude SDK's `tool_decision` and `safety_event` are this as telemetry). Add a cause-depth cap for event-driven loops (iterate stops at 8).
12. **Large payloads stay out of the log.** Over a threshold, store blobs (tool output, files, images, raw bodies) in a content-addressed blob table or object store and put `{ref, sha256, size, media_type}` in the payload (Temporal claim-check at 2 MB, OTel external content, Claude `body_ref`).

**Live subscription**

13. **Catch up, then go live, without a gap.** A subscriber gives `after_seq`. The actor serves stored events from the store, then switches to its broadcast channel. Every delivery carries `seq`, so the client can detect a gap and re-read. Signal "caught up" separately from "closed" (Durable Streams `upToDate` vs `streamClosed`). A lagging `tokio::sync::broadcast` receiver re-reads from the store.

**Telemetry derived from the log**

14. **Telemetry is a projection.**
    - Spans are opened and closed as events are *committed*, never during replay or catch-up (Temporal: "Spans are only exported when workflows actually complete, not during replays").
    - Mapping: the run maps to `invoke_agent {agent}` (INTERNAL). Each model request maps to `chat {model}` (CLIENT, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.id`, finish reasons, usage). Each tool call maps to `execute_tool {tool}` (INTERNAL, `gen_ai.tool.call.id` = `call_id`).
    - Set `gen_ai.conversation.id` to the instance or session ID.
    - Use `tracing` fields `otel.name` and `otel.kind`, and declare settle-time fields as `Empty`.
15. **Content capture is off by default.** Message, argument and result content goes on spans only under an explicit opt-in mirroring `OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT`. Otherwise record only IDs, sizes and blob refs, so the log stays the content store.
16. **Deterministic trace IDs.** Derive span IDs from `(instance_id, seq)` and store `trace_id` and `span_id` on the intent event, so a resumed run continues the same trace (Temporal's consistent span IDs; Claude SDK `TRACEPARENT` in, `session.id` attribute).

**Envelope fields**

17. **Minimum envelope.**
    - `instance_id` (stream).
    - `seq: u64`: dense, gapless per instance, assigned by the store and used as the offset.
    - `event_id: Uuid` (v7).
    - `type`: namespaced, for example `keel.tool.call_started`.
    - `schema_version`.
    - `payload` (JSON or JSONB).
    - `recorded_at`, a store timestamp. It is informational and never read by reducers.
    - `idempotency_key: Option`.
    - `causation_id`, the event or seq that caused this one, plus `correlation_id` / `run_id` / turn.
    - `actor`: user, model, tool, system or a sub-agent ID.
    - `trace_ctx: Option<{trace_id, span_id}>`.
    - Optional `writer_epoch`.
    - Ephemerals share the shape but have no `seq` (or a separate transient counter) and no row.

---

## Open questions

- The PLDI/PAgE paper text and its Python code were not found. The admit/propose semantics above come from the abstract only.
- The exact replay behavior of the workshop's `PullProcessorRuntime` (`afterAppend` during catch-up) could not be checked. The package is served from `pkg.pr.new`, and current `main` has replaced it with `processEvent` and the engine in [S3].
- Restate does not document exactly when a `ctx.run` result is acknowledged relative to continuing. The at-least-once body is inferred.

## Sources

- [S1] AI Engineer talk page, "Make your own event-sourced agent harness": https://www.ai.engineer/talks/vi-2nasppAg-make-your-own-event-sourced-agent-harness
- [S2] iterate workshop: https://github.com/iterate/ai-engineer-workshop/blob/main/workshop.md and `examples/cheat-sheet-2.ts` (cloned 2026-10-27)
- [S3] iterate monorepo @13b90be (2026-10-02): `core/lib/src/stream/processor.ts`, `core/lib/src/agents/processor.ts` — https://github.com/iterate/iterate
- [S4] Durable Streams README: https://github.com/durable-streams/durable-streams
- [S5] Durable Streams PROTOCOL.md: https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md
- [S6] PAgE @ PLDI 2026 program entry: https://pldi26.sigplan.org/details/page-2026-papers/9/Event-based-Design-Abstractions-for-Agent-Harnesses
- [S7] Temporal, Events and Event History: https://docs.temporal.io/workflow-execution/event
- [S8] Temporal, Workflow definition / determinism: https://docs.temporal.io/workflow-definition
- [S9] Temporal, Activity definition (idempotency): https://docs.temporal.io/activity-definition
- [S10] Temporal, blob size limit: https://docs.temporal.io/troubleshooting/blob-size-limit-error
- [S11] Temporal AI cookbook, OpenAI Agents SDK: https://docs.temporal.io/ai-cookbook/openai-agents-sdk-python
- [S12] temporalio-openai-agents README: https://github.com/temporalio/ai-integrations/tree/main/python/openai_agents
- [S13] Restate key concepts: https://docs.restate.dev/foundations/key-concepts
- [S14] Restate TS durable steps: https://docs.restate.dev/develop/ts/durable-steps.md
- [S15] Restate durable execution concept: https://docs.restate.dev/concepts/durable_execution
- [S16] Restate agent SDK integration guide: https://docs.restate.dev/ai/sdk-integrations/integration-guide.md
- [S17] Restate streaming responses: https://docs.restate.dev/ai/patterns/streaming-responses.md
- [S18] Restate observability and control: https://docs.restate.dev/ai/patterns/observability-control.md
- [S19] LangGraph checkpointers: https://docs.langchain.com/oss/python/langgraph/checkpointers.md
- [S20] LangGraph interrupts: https://docs.langchain.com/oss/python/langgraph/interrupts
- [S21] OTel GenAI semconv (moved notice): https://opentelemetry.io/docs/specs/semconv/gen-ai/
- [S22] OTel GenAI semconv repo @e19717d (2026-10-27): https://github.com/open-telemetry/semantic-conventions-genai (`docs/gen-ai/{gen-ai-agent-spans,gen-ai-spans,client-inference,gen-ai-events}.md`)
- [S23] tracing-opentelemetry 0.34 docs: https://docs.rs/tracing-opentelemetry/latest/tracing_opentelemetry/
- [S24] OpenAI Agents SDK tracing: https://openai.github.io/openai-agents-python/tracing/
- [S25] OpenAI Agents SDK processor interface: https://openai.github.io/openai-agents-python/ref/tracing/processor_interface/
- [S26] Claude Agent SDK observability: https://code.claude.com/docs/en/agent-sdk/observability
- [S27] Claude Agent SDK sessions: https://code.claude.com/docs/en/agent-sdk/sessions
- [S28] Perplexity SPACE architecture: https://github.com/perplexityai/SPACE/blob/main/docs/architecture.md
- [S29] Perplexity blog, "Making SPACE": https://www.perplexity.ai/hub/blog/making-space-secure-and-efficient-runtimes-for-long-running-agents
- [S30] **[unverified]** The New Stack: https://thenewstack.io/perplexity-space-agent-sandboxes ; TestingCatalog: https://www.testingcatalog.com/perplexity-launches-space-runtime-for-ai-agent-tasks/
