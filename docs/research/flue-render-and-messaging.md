# Flue's render rhythm and how an application messages an agent instance

How often Flue runs an agent function, what its durable log keeps about each render, and every way app code sends
a message to an instance and reads the reply; compared with keelrs. Sources: Flue `v2.3.0` at `deee93b4`
(paths relative to that repo, under `packages/runtime/src/` unless they start with `packages/` or `examples/`),
keelrs `main` at `29b20b5`. Each claim cites `path:line` read at those commits.

## 1. Render rhythm

**The agent function runs once per harness initialization, then again before every later model call.**

- One harness per submission attempt: `openAgentSubmissionSession` calls `ctx.initializeRootHarness(agent, ...)`
  for each submission (`runtime/agent-submissions.ts:1391-1401`); the state buffer is "one per harness lifetime
  (one submission attempt)" (`client.ts:304-306`, `hooks/use-persistent-state.ts:117-119`).
- First render: `renderAgentFunctionWithStructure(agent, renderState)` (`client.ts:331`) feeds turn 1.
- Per-turn re-render: the `rerender` closure (`client.ts:459-472`) is wired as the loop's
  `prepareNextTurnWithContext` (`session.ts:2299-2300`) and runs in `prepareRerenderTurn` (`session.ts:939-1004`),
  "at each turn boundary after the tool batch commits" (`session.ts:450-456`).
- Extra re-renders outside the turn loop: when a dispatch joins the live response, before its `useAgentStart`
  hooks (`session.ts:1472-1476`); before `useAgentFinish` hooks, since "the loop only re-renders between turns of one
  run, never after its last turn" (`session.ts:1649-1652`); at compaction, twice (`session.ts:1260`, `1276`).
  `useDelivery`'s doc sums it up: "Renders happen before every model call — each turn, and the moment a delivery
  joins the live response" (`hooks/use-delivery.ts:37-41`).
- Subagents render separately, once per delegation (`hooks/render.ts:139-175`).
- A render is synchronous and pure: a returned promise throws (`hooks/render.ts:238-249`), and a state setter called
  during render throws (`hooks/use-persistent-state.ts:86-90`).

### Per-submission vs per-turn values

| Value | Hook | Read when | Source |
|---|---|---|---|
| Model, `thinkingLevel`, `compaction` | `useModel` (exactly once per render) | first render only; a later render's value applies next submission | `client.ts:332-347`, `client.ts:445-447`, `hooks/use-model.ts:21-28`, `40-44` |
| MCP connections | `useMcpConnection` | first render only | `client.ts:348-360` |
| Sandbox factory and `cwd` | `useSandbox` (at most once) | first render; per turn only its **presence** is checked, and a flip swaps the environment at the turn boundary. Replacing one sandbox with another while present waits for the next submission | `client.ts:334-336`, `session.ts:1023-1053`, `hooks/use-sandbox.ts:24-43` |
| Tools | `useTool` | every render (fresh closures over current state) | `client.ts:467`, `session.ts:952`, `973` |
| Instructions / system prompt | return value + `useInstruction` | every render, recomposed into the leading system message | `client.ts:466`, `session.ts:974`, `1012-1021` |
| Skills, subagents (live sets) | `useSkill`, `useSubagent` | every render; the prompt's skill catalog and agent roster stay frozen at the durable baseline until compaction | `session.ts:953-958`, `client.ts:426-437` |
| Lifecycle hooks | `useAgentStart/Finish`, `useResponseStart/Finish` | every render (replaced wholesale) | `hooks/render.ts:89-98` |
| Persistent state | `usePersistentState` | every render reads snapshot + this attempt's writes | `hooks/use-persistent-state.ts:81-83`, `152-163` |

### What the invariance guard enforces

`assertRenderStructureInvariance` (`hooks/render.ts:190-201`, called at `client.ts:461`) compares only
`messageDataNames`, the `useDataWriter` names: adding or removing one between renders throws and fails the run.
Everything else may be conditional: resources (narrated instead), `usePersistentState` names (keyed by name), event
hooks, the sandbox (`hooks/render.ts:51-68`, `177-189`). Other per-render checks are local: one `useModel`
(`hooks/use-model.ts:40-44`), unique tool names (`hooks/render.ts:227-236`), unique state names
(`hooks/use-persistent-state.ts:74-78`). The support-desk example's comment ("Hook calls are never conditional: the
runtime enforces structural invariance across renders", `examples/support-desk/src/agents/support.ts:126-128`)
states a stricter rule than the code enforces.

### How props, `id`, `initialData` and the delivery reach a render

All through `RenderStateContext` (`hooks/frame.ts:37-74`), built once per harness (`client.ts:322-330`) and reused
by every re-render:

- `props.id`: the only prop, the instance id (`hooks/render.ts:15-33`); subagents get no props.
- `useInitialData()`: from the birth record admission wrote, already schema-parsed, constant for the instance's
  life; data on later messages is ignored (`client.ts:308-315`, `hooks/use-initial-data.ts:23-48`).
- `useDelivery()`: a cursor over the input the model is answering; it advances when a delivery joins
  (`client.ts:318-321`, `499-501`) and skips framework narration signals (`hooks/use-delivery.ts:30-46`).

### Narration of changes

After each re-render the session diffs the render's `ResourceSnapshot` against the durable last-narrated one and
steers `resources` signals (per changed kind) and an `instructions` signal (announcement only, digest changed) into
the transcript (`session.ts:1115-1180`). A sandbox presence flip instead narrates one `environment` signal restating
cwd and the full rosters (`session.ts:1055-1113`). These signal types are reserved framework vocabulary
(`conversation-records.ts:120-143`).

## 2. What the durable log records about renders, and what resume does

**There is no render record.** The `ConversationRecord` union (`conversation-records.ts:441`) has no entry per
render. What is stored:

- `resource_snapshot { baseline, snapshot }` (`conversation-records.ts:435-439`). A `ResourceSnapshot` holds names,
  descriptions and a tool's JSON schema string for skills, tools and subagents, plus an FNV digest of the
  instruction document, never its text (`resources.ts:37-58`, `hooks/render.ts:131-137`). It is appended **only on
  change**: first contact as a silent baseline (`session.ts:1136-1139`), with a delta's signals (`session.ts:1178`),
  with an environment swap (`session.ts:1076`, `1107-1111`), and at compaction, in the compaction record's batch
  (`session.ts:1262`). No delta, no record (`session.ts:1143`).
- The reducer folds snapshots into `{ narrated, baseline }`; `baseline` moves only on `baseline: true`
  (`conversation-reducer.ts:887-895`).
- The model appears only per assistant message, as `assistant_message_started.modelInfo`
  (`conversation-records.ts:150-154`).
- Not stored: the system prompt text, the sandbox factory or cwd, thinking level, compaction config, the tool
  closures.

**Resume runs the agent function again.** Every submission attempt initializes a new harness, which renders
(`client.ts:331`) and recomposes the system prompt live (`client.ts:440`). The durable snapshots are used only to
freeze the presentation surfaces (skill catalog, agent roster: `client.ts:430-437`) and as the diff base for
narration (`client.ts:473-476`, `session.ts:2263`). `rebuildCanonicalContext` rebuilds the transcript from records
and re-materializes the leading system message "from the current rendered prompt and executable tools"; the
historical position of mid-conversation tool declarations is not preserved (`session.ts:4902-4921`,
`1164-1176`). Determinism comes from the render reading only durable inputs: state replays from `state_write`
records, `useDelivery` "derives the same cursor from the durable record stream" (`hooks/use-delivery.ts:42-43`),
and a state-derived sandbox condition "replays durably" (`hooks/use-sandbox.ts:37-40`).

**`usePersistentState` writes.** A setter pushes into the per-attempt `HookStateBuffer`, dropping writes equal to the
current value (`hooks/use-persistent-state.ts:117-150`). The session drains the buffer into `state_write { name,
value }` records (`session.ts:2107-2116`, `conversation-records.ts:331-343`) in the **same append** as the tool
batch's `tool_results_committed` (`session.ts:2696-2715`): if recovery settles that batch as interrupted, the writes
never happened. A turn with no tool batch appends stray writes alone (`session.ts:2723-2726`); lifecycle hooks
commit theirs with their own batch (`session.ts:1500-1510`, `1660-1689`). The reducer keeps the last value per name
(`conversation-reducer.ts:884-886`).

## 3. Messaging an instance

### From app code

No `ask` or send-and-await call exists in either the runtime or the SDK; every path is dispatch, then read.

| Call | Signature | Returns |
|---|---|---|
| `dispatch` (runtime, top level) | `dispatch(agent: Agent, request: AgentDispatchRequest): Promise<DispatchReceipt>` (`runtime/flue-app.ts:71-74`) | at admission; "does not wait for model processing … To await the settled reply, use the `init()` handle" (`runtime/flue-app.ts:51-61`) |
| `init` | `init(agent: Agent, options?: InitOptions): AgentInstanceHandle` (`agent-client.ts:183`) | a handle, lazily bound to the runtime (`agent-client.ts:178-181`) |
| `handle.dispatch` | `dispatch(request: string \| AgentHandleDispatchRequest): Promise<DispatchReceipt>` (`agent-client.ts:137`) | at admission; request is `{ message, initialData?, idempotencyKey? }` or a string (`agent-client.ts:130-136`) |
| `handle.read` | `read(target: string \| DispatchReceipt, options?: AgentReadOptions): Promise<AgentReply>` (`agent-client.ts:155`) | `AgentReply { text, data, metadata?, uid?, submissionId }` (`agent-client.ts:96-106`); rejects with `AgentRunError` on failed/aborted (`agent-client.ts:108-123`); re-attachable from any process, `onEvent` streams chunks, `signal` cancels the read only (`agent-client.ts:83-93`, `139-154`) |
| `handle.abort` | `abort(): Promise<void>` (`agent-client.ts:162`) | once the abort intent is recorded |
| SDK over HTTP | `createFlueClient(options): FlueClient` (`packages/sdk/src/client.ts:164`), one client per conversation URL (`packages/sdk/src/client.ts:50-59`) | |
| `client.send` | `send(options: AgentPromptOptions): Promise<AgentSendResult>` (`packages/sdk/src/client.ts:64`) | 202 admission |
| `client.read` | `read(target: AgentSendResult \| string, options?): Promise<AgentReadResult>` (`packages/sdk/src/client.ts:75`) | the settled reply; composes `wait`, `history`, `readSubmissionReply` (`:65-74`) |
| `client.wait` | `wait(admission, options?): Promise<void>` (`packages/sdk/src/client.ts:83`) | outcome only, no reply (`:76-82`) |
| `client.abort`, `history`, `historyBefore`, `observe` | `packages/sdk/src/client.ts:90-100` | |

A reply after `dispatch` is read with `handle.read(receipt)` (or the bare `submissionId`), or over HTTP with
`client.read`, or `wait` then `history`. Structured output is not offered on any app-facing call: `dispatch`
carries a message, and the reply has `text` plus named `data` parts written by `useDataWriter`
(`agent-client.ts:99-100`).

### Inside a run

`harness.prompt` (`harness.ts:199-208`, typed at `types.ts:746-750`), `session.prompt` (`session.ts:2780-2790`,
`types.ts:805-809`), `session.skill` (`types.ts:825-829`) and `session.task` (`session.ts:3741-3748`,
`types.ts:836-840`) are for code running inside an agent (a tool with `harness: true` gets `harness`). Each returns
`CallHandle<T>`: a `Promise<T>` with `signal` and `abort()` (`types.ts:781-792`). Plain calls resolve to
`PromptResponse { text, usage, model }`; passing `{ result: valibotSchema }` selects the overload resolving to
`PromptResultResponse<T> { data, usage, model }` with validated `data` (`types.ts:903-919`, `924-926`). The schema
flow injects `finish` and `give_up` tools for the call (`session.ts:589-594`). `harness.prompt` runs on the
harness's default session (`harness.ts:204-207`).

### Call sites

- Fire-and-forget dispatch from a cron: `await dispatch(Scheduled, { id: 'daily-summary', message: { kind:
  'signal', ... } })` (`examples/node-schedules/src/app.ts:22-30`).
- SDK send, wait, history: `conversation.send({ message: { kind: 'user', body: input }, signal })`, then
  `conversation.wait(admission, { signal })`, then `conversation.history({ signal })` and the last assistant message
  (`examples/vitest-evals/src/evals/harness.ts:110-121`).
- Structured prompt inside a tool: `harness.prompt('What is 2 + 2? ...', { result: v.object({ answer: v.number()
  }) })`, read as `response.data.answer` (`examples/hello-world/src/agents/hello.ts:20-31`).
- `init` + `dispatch` + `read` appears only in the doc example (`agent-client.ts:168-176`); no example uses it.

## Comparison

| | Flue | keelrs |
|---|---|---|
| Render rhythm | once at harness init per submission attempt, then before each later model call; also at joins, before `useAgentFinish`, at compaction (`client.ts:331`, `session.ts:939`, `1476`, `1652`, `1260`) | before every model call: `agent.render()` in `AgentRunStep::CallModel` (`keelrs/src/agent/rig/mod.rs:107-111`); GLOSSARY "Render" |
| Per-submission values | model, thinking, compaction, MCP, sandbox identity and cwd (`client.ts:332-360`) | model and thinking: connected on the first render of a dispatch and kept (`rig/mod.rs:93-94`, `126-129`); later renders still must declare a model (`rig/mod.rs:112`) |
| Per-turn values | tools, instructions, live skills/subagents, lifecycle hooks, sandbox **presence** | tools, instructions, skills, the one sandbox (ADR 0006), built-in names (`rig/mod.rs:112-125`); a different sandbox id empties the built-ins' cache (ADR 0006) |
| Required each render | `useModel` once; sandbox optional, at most once | `use_model` and exactly one `use_sandbox` (ADR 0006) |
| Cross-render guard | message-data names must not change (`hooks/render.ts:190-201`) | none |
| Change narration | `resources` / `instructions` / `environment` signals (`session.ts:1055-1180`) | none (see `agent-loops.md`) |
| Log per render | nothing per render; `resource_snapshot` only on change (names, descriptions, schema strings, instruction digest); model per assistant message; no prompt text | no event log yet; issue #47 open |
| Resume | re-renders the agent function; snapshots only freeze catalogs and feed the diff (`client.ts:430-476`) | open (issue #49) |
| State writes | `state_write` in the tool batch's append (`session.ts:2696-2715`) | none (instance data only, frozen at creation, per #33) |
| Send, get receipt | `dispatch(agent, req)`, `handle.dispatch(req)`, SDK `send` | `AgentInstance::dispatch(msg) -> Result<Receipt>` (`keelrs/src/agent/instance.rs:69`) |
| Read reply | `handle.read(receipt \| id)`, SDK `read`, or `wait` + `history` | `read(&Receipt) -> Result<Reply>` (`instance.rs:104`) |
| Send and await | none | `ask(msg) -> Result<Reply>` (`instance.rs:75`) |
| Structured, from app | none; the reply carries `text` + named `data` parts | `ask_structured::<T>(msg) -> Result<T>` (`instance.rs:83`) |
| Structured, inside a run | `prompt/skill/task(text, { result })` → `.data` | none (the agent is the only caller of the model) |
| Abort | `handle.abort()`, SDK `abort` | not on `AgentInstance` |

## For keelrs (inference)

These are conclusions drawn from the facts above, not facts about either codebase.

- **Flue does not make every model request rebuildable from its log.** It stores neither the prompt text nor each
  render's tools; resume re-runs the agent function and relies on the render being a pure function of durable
  inputs (state records, initial data, the delivery cursor). That contradicts the "model-visible means logged" rule
  the canonical-event research found in DeepSeek Harness (`research/canonical-event-contents`). Issue #47 asks
  exactly this question; Flue is evidence that the cheaper answer works only if renders are deterministic, which
  keelrs cannot enforce (a Rust closure can read anything).
- **A render event should record what the render declared, at least what the model saw.** keelrs re-renders every
  turn and pins the render's tools for the `CallTools` step that follows (`rig/mod.rs:142-148`); a resumed run that
  re-renders may see different tools than the model was offered. Recording per render: model id (and thinking),
  the advertised tool names with their schemas (or a digest, as Flue's `ResourceSnapshot` does), skills, the
  composed instructions (text, or a digest plus the text when it changes), and the sandbox by its durable key.
- **Record on change, not per render, to keep the log small.** Flue's `resource_snapshot` is appended only when the
  diff is non-empty; keelrs could do the same (a render event carrying only what changed since the last one) and
  still rebuild every request, provided the first render of a dispatch records the full set.
- **The model is effectively per-dispatch in keelrs too**: `connected` is set once per `run_with`
  (`rig/mod.rs:126-129`), so a later render's different model is ignored silently. Either the render event records
  the model only on the first render, or keelrs rejects/handles a change; Flue documents the same choice
  (`hooks/use-model.ts:25-27`).
- **State writes belong in the tool batch's event.** If keelrs adds persistent state, Flue's rule (writes commit
  atomically with the tool results that made them) keeps a crashed batch from leaving half its effects.
- **Messaging surface**: keelrs's `ask` and `ask_structured` are additions Flue lacks; Flue's reads are
  re-attachable by submission id from any process and stream events (`onEvent`), which keelrs's `read(&Receipt)`
  would need once events are durable (issues #47, #49). Flue also offers `abort` on the handle.
