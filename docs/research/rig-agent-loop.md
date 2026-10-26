# Research: Rig's agent loop against what Flue needs from Pi

Ticket: #8 (map #7). Date: 2026-10-26.

Sources:
- **Rig** `0xPlaygrounds/rig` at tag `v0.45.0` (commit `98ed06d`, 2026-10-19), the latest stable release. `rig`, `rig-core`, `rig-agent` and `rig-ecs` are all 0.45.0 on crates.io (published 2026-10-23); `v0.46.0-rc.1` is a prerelease and was not read. Paths below are relative to the repo root at that tag.
- **docs.rs** for `rig-agent` 0.45.0 and `rig-core` 0.45.0.
- **Flue** `withastro/flue` at `deee93b` (2026-10-12), `packages/runtime/src/session.ts`. Flue's Pi is `@earendil-works/pi-agent-core` `^1.1.0` (`packages/runtime/package.json:87`).

This note gathers facts for the later decision ticket. It does not make the decision.

---

## 0. Where Rig's agent loop lives now

The layout has changed a lot since older Rig releases:

- **`rig-core`**: the provider contracts, messages and `Model` / `DynModel` (`call`, `stream`). It has no agent loop.
- **`rig-agent`** ("Rig's classic agent runtime", `crates/rig-agent/Cargo.toml`): `Agent`, `AgentBuilder`, `AgentRunner`, the hooks and the streaming. The `rig` facade re-exports it as `rig::agent` under the default `agent` feature (`src/lib.rs:4-8, 47-81`). It has two layers:
  - **`rig_agent::run::AgentRun`**: a serializable state machine with no IO (sans-IO). It owns every loop decision (turn budget, tool-call validation and recovery, history threading, output policy, usage) and returns `AgentRunStep::{CallModel, CallTools, Done}` for a driver to act on (`crates/rig-agent/src/run/mod.rs:1-12, 75-100`; README "The run protocol", lines 78-103). It is `Serialize + Deserialize`, so it can be suspended between steps and resumed in another process ([docs.rs](https://docs.rs/rig-agent/0.45.0/rig_agent/run/struct.AgentRun.html)).
  - **`AgentRunner`**: the driver that ships with the crate. It runs `AgentRun` with hooks, tools, memory and streaming (`crates/rig-agent/src/agent/runner.rs:39-63`, [docs.rs](https://docs.rs/rig-agent/0.45.0/rig_agent/agent/runner/struct.AgentRunner.html)).
- **`rig-ecs` + `rig-harness`**: Rig's own terminal coding agent. It is built on Bevy, and **it does not use `rig-agent`**: both crates depend only on `rig-core` (and `rig-memory`) (`crates/rig-ecs/Cargo.toml` `[dependencies]`; `crates/rig-harness/Cargo.toml:34`). It has its own turn loop (`crates/rig-ecs/README.md`: "`turn`: the turn loop") and its own steering (`DeliveryMode::{Steer, Queue, Inform}`, `crates/rig-ecs/src/inbox.rs:61-74`).

So "Rig's agent loop" means `rig-agent`'s `AgentRunner` over `AgentRun`. Section 7 compares three ways to use it.

---

## 1. A hook between turns that swaps model, system prompt and tools

**What Flue does:** Flue sets `prepareNextTurnWithContext` (`session.ts:2296-2302`) to `prepareRerenderTurn` (`session.ts:939-1004`). That function runs after a tool batch commits. It re-runs the agent function, may swap the sandbox, rebuilds the tool list (tools can be **added**, not only removed, with fresh closures), rewrites the system prompt, steers narration messages, and returns a new `{messages, tools}` context for the next provider request.

**Rig:** **workable**. One async hook covers the prompt and narrowing the tools. Swapping the model and adding tools need extra plumbing.

| Need | Rig mechanism | Verdict |
|---|---|---|
| Run code before each model call | `AgentHook::on_completion_call` is **async** and runs before every completion request, including after tool batches and on retries. It receives `CompletionCallEvent { prompt, history, turn }` (`agent/hook.rs:337-345, 983-993`). | supported |
| Swap the system prompt | It returns `CompletionCallAction::patch(RequestPatch)`. `RequestPatch.preamble` replaces the preamble for that turn only (`run/patch.rs:15-47`, [docs.rs](https://docs.rs/rig-agent/0.45.0/rig_agent/run/patch/struct.RequestPatch.html)). The patch is **non-sticky**: it is not carried into later turns, so the hook has to set it on every turn. That fits an agent function that renders every turn. | supported |
| Narrow the tools | `RequestPatch.active_tools` is an allow-list. When several hooks set it, the lists are intersected (`run/patch.rs:39-40, 160-171`). | supported |
| **Add** or replace tools mid-run | A patch cannot add tools. Tools live in a shared, mutable registry, `ToolServerHandle` (`Arc<RwLock<…>>`, with `add_tool` / `remove_tool` / `add_tools` taking `&self`, `agent/tool/server.rs:284-312, 437-448`). The engine snapshots the registry when it prepares each turn's request (`agent/engine.rs:328-349`, "Pin output mode across registry changes between turns"). A hook that holds the handle (`Agent::tool_server_handle()`, `agent/completion.rs:418`) can re-register tools before the request is prepared. A replaced tool gets a new generation key, and a request still runs the version it advertised (README lines 218-224). | workable |
| Swap the model | `AgentHook::on_model_select` runs **after** `on_completion_call` at every model call and returns `ModelSelectionAction::select(label)` (`agent/hook.rs:355-371, 971-981`; README "Runtime model routing", lines 108-137). It can only pick a `ModelRef` label **already registered** in the agent's handler table, and it is **synchronous**. `Handlers::register(&self, …)` is public (`handlers.rs:180`) and the agent exposes `handlers()` (`agent/completion.rs:498`), so a hook could register a newly built model under a label first and then select it. That path is not shown in Rig's examples or tests and is unverified. | workable |
| Swap the sandbox / environment | Rig has no such concept. keelrs would own it. | n/a |

To fit a keelrs agent function into this, the hook would:
1. In `on_completion_call`, run the agent function for this turn.
2. Re-register the tool changes on the `ToolServerHandle`.
3. Put the model label in the run's `Scratchpad` (`agent/hook.rs:62-130`).
4. Return a `RequestPatch` with the preamble and active tools.
5. In `on_model_select`, read the label back from the scratchpad.

A hook-level `Stop` fails the run cleanly, which fits Flue's "an invariance violation throws here and fails the run".

## 2. Read and write access to message history

**What Flue does:** It reads `agentLoop.state.messages` freely and also **assigns** it, both between runs and from inside the per-turn hook (`session.ts:1000-1020, 4921`). The system prompt is stored as the first message of that history.

**Rig:** **workable**. Reads are good. A run's committed history cannot be written from outside or from a hook.

- **Before a run (supported):** `AgentRunner::history(…)` sets the history that comes before the prompt (`agent/runner.rs:225-234`). Memory stores (`AgentBuilder::memory`) are optional. A run that fails or is cancelled saves nothing to memory (README line 29).
- **After a run (supported):** `PromptResponse.messages` holds the run's own transcript (prompt, accepted assistant turns, tool results). It excludes the input history (`run/response.rs:80-109`). On cancellation, `PromptError::Cancelled { chat_history, reason }` returns the history with every open tool call closed (`run/response.rs:276-284`). `MaxTurns` also carries `chat_history`.
- **During a run, reads (supported):** every hook event carries the history (`CompletionCallEvent.history`, `ModelSelection.history`, `RunSettled.messages`). On `AgentRun`, `messages()`, `full_history()` and `canonical_history()` read it (`run/mod.rs:632, 734, 778`).
- **During a run, writes (workable, awkward):** `RequestPatch.history` replaces what is **sent** for one turn. It does not change the stored conversation (`run/prepare.rs:260-263`: "Request history replacement must not mutate persisted conversation history"). A compaction mid-run would therefore have to be re-applied on every later turn of that run. `AgentRun` has no public method to rewrite or append to committed history. History is set only when the run is built (`with_history`, `with_validated_history`; `run/mod.rs:486, 1757`).
- **Restore (supported, with a caveat):** `Agent::resume(AgentRun)` continues a run that was serialized mid-flight (`agent/completion.rs:633-645`). The caveat: "Resume with the same Rig version; … cross-version state compatibility is not guaranteed" (README line 88). That makes serialized `AgentRun` JSON a poor long-term durable format. Durable keelrs state would sit better as plain messages.

## 3. Event subscriptions

**What Flue does:** It subscribes to `agentLoop.subscribe` and switches on `agent_start`, `turn_start`, `message_start/update/end` (text and thinking deltas), tool execution start and end, `turn_end`, and `agent_end` (`session.ts:2305` onward).

**Rig:** **supported**, through two surfaces.

| Flue/Pi event | Rig hook (`AgentHook`, [docs.rs](https://docs.rs/rig-agent/0.45.0/rig_agent/agent/hook/trait.AgentHook.html)) | Rig stream item (`MultiTurnStreamItem`, [docs.rs](https://docs.rs/rig-agent/0.45.0/rig_agent/agent/enum.MultiTurnStreamItem.html)) |
|---|---|---|
| agent start | `on_run_start` (can also rewrite or stop) | none |
| turn start | `on_completion_call` (one per model call) | none |
| text / thinking delta | `on_text_delta`, `on_reasoning_delta` | `StreamAssistantItem` |
| tool call args streaming | `on_tool_call_delta` | `StreamAssistantItem` |
| assistant turn end | `on_model_turn_finished` (can retry or stop) | `CompletionCall` (usage) |
| tool execution start | `on_dispatch` for the tool family (live; can deny, patch or skip) | none live: `ToolExecutionCommitted` arrives **after** the batch settles |
| tool result | `on_outcome` for the tool family (live; can replace the output) | `ToolResult` (after the batch settles) |
| finish | `on_run_settled` | `FinalResponse`, or the stream's `Err` |
| turn retried | none | `ModelTurnRetried` (discard provisional deltas) |

Sources: `agent/hook.rs:928-1115`, `agent/streaming.rs:40-127`.

Live per-tool start and end therefore come only from hooks. The stream items are a projection of committed history (`agent/streaming.rs:48-60, 66-74`). `run_channel` returns a future plus a bounded event feed. A full feed applies backpressure to the run (`agent/streaming.rs:296-312`). There is no "tool batch end" or "turn end after tools" event. The next `on_completion_call` is the nearest boundary.

## 4. Steering and abort

**What Flue does:** `agentLoop.steer(message)` queues a message that the loop delivers at the next turn boundary, before the next provider request. Flue sets `steeringMode: 'all'` so that everything queued is delivered together (`session.ts:2286-2295, 1380, 1807`). `agentLoop.abort()` cancels the run (`session.ts:3799`).

**Steering: not supported** as a primitive in `rig-agent`. It is **workable**:
- `rig-agent` has no steer or inject API. A repo-wide search for steer/inject/follow_up/enqueue functions finds nothing in `rig-agent`. `AgentRun` cannot have a user message appended after it is built (section 2), and a non-sticky `RequestPatch.history` would drop the message on the next turn.
- `ModelTurnAction::retry_with_feedback` appends a user message, but only to **retry a tool-free turn** (`agent/hook.rs:425-460`). That is a retry, not steering.
- Workable pattern 1: a hook in `on_completion_call` (turn > 1) checks a queue, returns `Stop`, and the caller gets `Cancelled { chat_history }`. The caller then starts a new run with `.history(chat_history)` and the queued messages as the prompt. Usage totals and the turn budget restart, and `on_run_start` fires again.
- Workable pattern 2: an own driver over `AgentRun` does the same thing at `CallModel` steps, building a new `AgentRun::new(steer).with_history(full_history)`.
- Rig's own coding agent has steering, but in `rig-ecs`, not `rig-agent`. `DeliveryMode::Steer` means "with the running turn's next model call", `Queue` means "once the running turn would end", and `Inform` means "goes with the next model call, never makes one" (`crates/rig-ecs/src/inbox.rs:61-74`). These match Pi's steer and follow-up modes.

**Abort: supported.**
- Dropping the `run()` future or the `stream()` stream cancels the in-flight provider call and every tool dispatch under it (README lines 60-63, 134-137). No executor is involved.
- A hook can return `Stop` from almost any event, including from `on_text_delta` in the middle of a stream. The result is `PromptError::Cancelled` with a repaired history (`run/response.rs:276-284`).
- There is no external `abort()` handle. keelrs would wire one: a shared flag read by a hook gives a clean `Cancelled` with history, and dropping the future or a `select!` gives a hard cancel with no history back. Memory-backed runs save nothing when cancelled.

## 5. Multi-agent

**Partly supported.**
- **Agent as tool:** `Agent::into_tool()` wraps an agent as a `DynamicTool` that takes `{prompt}`, runs `agent.prompt(prompt)`, and returns the text output. The tool context is inherited (`agent/tool.rs:30-81`; examples `examples/agent_with_agent_tool`, `examples/multi_agent`). Each call is one blocking sub-run: the subagent's history is not kept between calls and its events are not streamed to the parent.
- **Many agents:** an `Agent` is a cheap, cloneable value. "Concurrent runs keep independent default and hook-stack snapshots" (README lines 174-180). The `ToolServerHandle` is shared across clones and across all runs of one agent unless one is given per agent (`AgentBuilder::tool_server_handle`, `agent/builder.rs:569`).
- **Not provided by `rig-agent`:** an addressable, long-lived conversation per id, delivery to a running agent, or parent/child task trees. In Rig those live in `rig-ecs` (agents as entities, `Deliver`, subagents as plugins: `crates/rig-ecs/README.md`; `plugins/rig-basics/src/subagents.rs`). keelrs's Runtime / Agent instance / Dispatch / Receipt / Read / Reply layer would be keelrs code either way.

## 6. Lifecycle friction with an agent function that re-runs every turn

Rig's model is to build the agent once (`AgentBuilder` → `Agent`), then run it. Each run is configured per call (`AgentRunner` overrides, hooks added per run, `agent/runner.rs:133, 201-260`). The friction points:

1. **Models are registered, not passed.** Per-turn model choice must name a label already in the handler table. A model the agent function produces fresh has to be registered first (section 1). Registered models and hooks are not serializable, so only the label is persisted (README lines 174-176).
2. **Tools are agent-scoped, not turn-scoped.** Adding tools mutates a registry shared by every run of that `Agent` value. Concurrent Agent instances would need one `Agent` (or one `ToolServerHandle`) each, built per Agent instance or per Dispatch.
3. **Two hooks, two timings.** `on_completion_call` is async and can run the agent function. `on_model_select` is sync and must read the result from the `Scratchpad`. The order is fixed: completion call first, then model select, then request preparation (README lines 122-125).
4. **No mid-run history writes and no steering.** Both of these push keelrs to end a run and start a new one at turn boundaries (sections 2 and 4). That breaks Rig's per-run accounting: usage, `max_turns`, `on_run_start` and `PromptResponse` each cover one run, not one Dispatch.
5. **First turn too.** `on_completion_call` also fires on turn 1, so the agent function can be the only source of model, prompt and tools. The `Agent` then holds only empty defaults plus the registered models.
6. **Version coupling.** Serialized `AgentRun` state is tied to one Rig version (README line 88). The crate is 0.x, `rig-ecs` reused an unrelated crate name at 0.45, and the API moves quickly.
7. **Term clash.** Rig uses "dispatch" for an effect going through its `Handlers` table (README "Handlers and dispatch"). In keelrs, Dispatch means submitting a message to an Agent instance. Code that touches both should qualify the term.

## 7. What this implies for "Rig's agent loop as the core" vs "own loop over Rig's completion model"

There are three shapes, not two:

| | A. `AgentRunner` + hooks | B. Own driver over `rig_agent::run::AgentRun` | C. Own loop over `rig_core::Model` |
|---|---|---|---|
| Per-turn prompt / tools / model | workable via `on_completion_call` + `on_model_select` + shared `ToolServerHandle` | direct: keelrs calls `prepare_request(spec, caps, history, tools, …, patch)` (`run/prepare.rs:121-128`) at each `CallModel` with whatever the agent function returned. Agent hooks do **not** run (README line 92). | direct |
| History read/write | read: yes; write: only per-turn patch, or by ending the run | read: yes; write: only by building a new `AgentRun` | full control |
| Events | rich hooks + stream items | keelrs emits its own around each step | keelrs emits its own |
| Steering | end run + restart | end `AgentRun` + new one at a `CallModel` step | native: append before the next call |
| Abort | drop, or hook `Stop` | keelrs owns the driver | keelrs owns it |
| What Rig still gives | tool-call validation and recovery, output policy, retries, streaming assembly, telemetry, memory | the same loop decisions (validation, recovery, output policy, usage, `StreamedTurnAssembler`) without hooks or IO; `rig-agent` with default features off carries no async runtime or transport (README lines 96-103) | providers, message types, streaming, tool contracts |
| Friction | highest (sections 1, 2, 4, 6) | medium: steering and mid-run history writes still mean rebuilding the run | lowest friction, most code to write |

Facts that bear on the decision:
- Every Flue need maps onto shape A as **workable**, but two of them (steering, mid-run history writes) only by ending and restarting runs. That is the same "restart the loop" shape Pi avoids.
- **Rig's own coding agent chose shape C.** `rig-ecs` and `rig-harness` run their own loop over `rig-core` and add steer, queue and inform delivery themselves.
- Shape B keeps Rig's careful tool-call recovery and output logic, and gives keelrs the turn boundary directly. Its gaps are the same as A's (no append, no history rewrite), but they are cheaper to work around because keelrs owns the driver.
- Whichever shape is chosen, keelrs owns the Runtime, Agent instance addressing, Dispatch / Receipt / Read / Reply, durable storage of messages, and the sandbox. None of these come from `rig-agent`.
