# Agent loops: keelrs, pi and Rig compared

How three agent loops call the model, run tool calls and change their setup between turns, and what keelrs could
borrow. Sources: pi `e7a0df27c` (`v1.4.0-211`), Rig `e1cd47d57` (`v0.46.0-rc.1-21`), keelrs `main` at `b5f020d`.
Facts below come from reading the code; the file:line references were not re-checked line by line.

## Where each loop lives

- **keelrs**: `run_with` in `keelrs/src/agent/rig/mod.rs`. It steps Rig's `AgentRun` itself (ADR 0004), re-running
  the agent function before every model call.
- **pi**: `runLoop` in `packages/agent/src/agent-loop.ts` (172-330): one async function, an outer `while (true)`
  around an inner `while (hasMoreToolCalls || pendingMessages.length > 0)`. No step API; per-turn hooks instead.
  Entry points `agentLoop`/`agentLoopContinue` (an `EventStream`) and `runAgentLoop`/`runAgentLoopContinue`
  (an `emit` callback); the `Agent` class (`packages/agent/src/agent.ts:446,459`) wraps them.
- **Rig** has two:
  - `rig-agent`: `AgentRun` (`crates/rig-agent/src/run/mod.rs:76`), a serialisable step machine that does no I/O,
    and its built-in driver `drive_agent` (`crates/rig-agent/src/agent/engine.rs:143`), reached through
    `AgentRunner::run()` (`agent/runner.rs:546`), `.stream()` and `Agent::resume(AgentRun)`. The driver and its
    tool helpers (`drive_tool_calls`, `run_single_tool`, `dispatch_tool_call`) are `pub(crate)`.
  - `rig-ecs` + `rig-harness`: the coding-agent harness, a Bevy ECS kernel with its own turn loop; it does not use
    `AgentRun`. The only place `Touches` footprints order calls (`rig-ecs/src/turn.rs` `wait_in_order`).

## Same in all three

- Setup is recomputed before every model call: keelrs re-runs the agent function; pi `prepareNextTurn` (from the
  second turn) and `prepareRequest` (every request); Rig `on_completion_call` (a `RequestPatch`: preamble, options,
  `active_tools`, history) and `on_model_select`.
- The tools the model was shown are pinned for the tool calls that follow (keelrs `pinned`, Rig
  `pending_tool_snapshot`; pi passes the context's tools through).
- An unknown tool, bad arguments or a tool error become a result the model reads, not a failed run.
- One reply's calls run concurrently and their results are committed in call order.

## Different

| | keelrs | pi | Rig `drive_agent` |
|---|---|---|---|
| Step by step | yes | no | inside the driver only |
| Tools per turn | built fresh by the agent function | `context.tools` swapped | registered up front; a patch narrows with `active_tools` |
| Model told tools changed | no | yes, `declareToolChanges` (342-372) | no |
| Concurrency | `join_all`, unbounded | `Promise.all`; sequential per config or per tool (`executionMode`) (518-533) | `buffer_unordered(n)`, default 1, fail-fast |
| Schema check before run | yes, `arguments_refusal` | yes, `validateToolArguments` | no, left to the tool |
| Tool timeout | per tool | none in the loop (shell tool has its own) | none |
| Reply cut at length limit | not handled | its tool calls fail, model asked to re-issue (488-513) | not found |
| Stop | max turns | no max in core (durable harness has `maxTurns`); `finishTurn` → `end`; all results `terminate` | max turns; any hook returns `Stop` |
| Mid-run messages | no | steering and follow-up queues (`getSteeringMessages`, `getFollowUpMessages`) | no |
| Events / hooks | none yet | `agent_start/end`, `turn_start/end`, `message_*`, `tool_execution_start/update/end` | `AgentHook`: `on_run_start`, `on_run_settled`, `on_model_select`, `on_completion_call`, `on_model_turn_finished`, `on_invalid_tool_call`, `on_*_delta`, `on_dispatch`, `on_outcome` |
| Tool context | `Sandbox` per call | captured at tool construction (`cwd`, `BashOperations`) | `ToolContext` copied per call (`for_dispatch`) |
| Tool hooks | none | `beforeToolCall` (block, terminate), `afterToolCall` (override result) | `on_dispatch` (patch, skip, stop), `on_outcome` |

## Why keelrs keeps driving `AgentRun` itself

Rig's driver wants tools registered in a tool server up front and only narrows them per turn; keelrs builds new
`Tool` closures every render, with a sandbox that may differ per turn. The driver is `pub(crate)`. And a
serialisable `AgentRun` stepped by keelrs is what durable runs over sqlx build on.

## What keelrs could borrow

1. **Advertised tools in the run** (Rig): `run.advertise_tools(turn, ..)` after each model call, so a resumed run
   rebuilds its pinned tools from `advertised_tools()` (`engine.rs:390-402`). For resume.
2. **A reply cut at the length limit** (pi): answer its tool calls with a re-issue request instead of running
   possibly truncated arguments. Check first that Rig's `ModelTurn` exposes the stop reason.
3. **Telling the model its tools changed** (pi `declareToolChanges`): a system note with added and removed tools
   when a render's tools differ from the last.
4. **Ordering and concurrency** (Rig `buffer_unordered(n)`, pi per-tool `executionMode: "sequential"`): options
   alongside footprints; see `research/tool-call-ordering`.
5. **Events, hooks, steering and stopping**: pi's event set and steering/follow-up queues, pi's `terminate` and
   `finishTurn`, Rig's `AgentHook` list and `Stop`: starting points for events and hooks.
