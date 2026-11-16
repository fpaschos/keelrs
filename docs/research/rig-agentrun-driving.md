# How drivers of Rig's `AgentRun` get each tool result as it finishes

Research for issue #110 (map: #43, blocks #111). It reads, from source, what Rig's `AgentRun` offers for one tool result at a time, how Rig's own drivers record per-call results, and how Flue maps its per-tool records back into the conversation. Sections 1 to 5 are facts with citations. **Section 6 is inference** for keelrs's `result_of` / `produced` code. It makes no decisions.

## Sources

- **`rig-agent` and `rig-core` `0.46.0-rc.1`**, the versions keelrs pins (`Cargo.toml:22-24`), read from the cargo registry source. `AGENT/` = `rig-agent-0.46.0-rc.1/`, `CORE/` = `rig-core-0.46.0-rc.1/`. The public `fn` list of `AGENT/src/run/mod.rs` is the same in `0.46.0` and in the local `../rig` checkout, and `run/batch.rs` and `run/committed.rs` are unchanged in `0.46.0`.
- **`rig-ecs`, `rig-harness`, `rig-memory`**: local `../rig` checkout at `286d897a9` (2026-11-12), 184 commits past `v0.46.0-rc.1`. `ECS/` = `../rig/crates/rig-ecs/`. Line numbers are that commit's.
- **Flue**: local `../flue` at tag `v2.3.0` (`deee93b4`). `FLUE/` = `../flue/packages/runtime/src/`.
- **keelrs**: `main` at `5ca6b63`. `RUN` = `keelrs/src/agent/rig/run.rs`.
- Earlier notes, not repeated here: branch `research/rig-agent-loop` (`docs/research/rig-agent-loop.md`) and branch `research/tool-call-ordering` (`docs/research/tool-call-ordering.md`).

## Summary

- **`AgentRun` gives nothing back per result.** `answer` returns `Result<(), PromptError>` (`AGENT/src/run/mod.rs:1360`). The result content it builds is held in private batch state until the last call is answered; only then does it reach `messages()` as one user message (`mod.rs:1434-1444`). There is no per-call event or hook on `AgentRun` (`mod.rs:3-5`: "A run owns no models, tools, or hooks").
- **The only read of a partial batch is `full_history()`**, which clones the whole conversation and closes the still-open calls with synthetic results (`mod.rs:734-756`, `batch.rs:73-93`).
- **A batch cannot be committed result by result.** `answer_all` commits only when every slot is answered (`mod.rs:1438-1444`, `batch.rs:60-71`). `ToolAnswer`'s fields are `pub(crate)`, and a driver cannot hand the run result content it built (`calls.rs:140-158`, a `compile_fail` doc test).
- **The content Rig commits is a pure, public function of the call and the `ToolResult`.** `answer_all` builds it with `rig_core::tool::feedback::tool_result_output` (`mod.rs:1397-1401`), and `ToolCall::answer` (`CORE/src/completion/message.rs:521-528`) has the same body. A driver can build the exact part before it calls `answer`.
- **The `produced` cursor is Rig's own documented pattern.** `project` + `projection_start` (`AGENT/src/run/committed.rs:53-90`, `mod.rs:636-644`), and Rig's engine keeps exactly such a cursor and moves it to the end after each step (`AGENT/src/agent/engine.rs:196-212`).
- **Rig's `AgentRunner` does not stream results one by one.** It drains the whole batch, then calls `answer_all` once (`engine.rs:513-600`). Per-call live data comes only from `AgentHook::on_dispatch` / `on_outcome` (`AGENT/src/agent/hook.rs:1075-1100`).
- **`rig-ecs` / `rig-harness` do not use `AgentRun`.** They depend on `rig-core` and `rig-memory` only (`ECS/Cargo.toml:30,33`; `../rig/crates/rig-harness/Cargo.toml:35-41`). Each settled call of an open reply gets a `rig.call` aside holding its own `ToolResult`; the reply's results are committed as one entry once all settle, and the marks are removed then (`ECS/src/turn.rs:1847-1886`, `ECS/src/restore.rs:72-223`).
- **Flue does the same in two record kinds.** One `tool_outcome` record per finished call, written from the loop's `tool_execution_end` event, which carries the result (`FLUE/session.ts:2590-2632`). One `tool_results_committed` record per batch, listing the outcome ids in call order (`FLUE/session.ts:2667-2715`). Only that commit turns outcomes into `toolResult` messages (`FLUE/conversation-reducer.ts:693-772`).
- **Neither Rig's kernel nor Flue reads a result back out of the conversation.** Both record the result the tool produced, keyed by call id, and build the message from those records.

## 1. What `AgentRun` offers for one tool result

### 1a. `answer` and `answer_all`

- `pub fn answer(&mut self, answer: ToolAnswer) -> Result<(), PromptError>` is `answer_all([answer])` (`AGENT/src/run/mod.rs:1358-1362`). It returns no content.
- `answer_all` (`mod.rs:1364-1446`):
  - Copies the batch from `RunState::ExecutingTools` (`mod.rs:1377-1381`).
  - For an executed call, builds `tool_result_output(call, tool_call.function.name.clone(), &result)` (`mod.rs:1397-1401`).
  - For a malformed call, builds `malformed_answer(tool_call, action)` (`mod.rs:1402-1422`).
  - Stores the content in the slot: `ToolSlot::Answered(content)` (`mod.rs:1434-1436`).
  - Commits only when `batch.complete()` succeeds: `self.history.commit([results])`; otherwise the state stays `ExecutingTools` (`mod.rs:1438-1444`).
- The doc says it in one line: "The run builds each result; once every call is answered they are committed as one user message in call order" (`mod.rs:1364-1366`).
- `ToolBatch::complete` returns the message only when every slot is `Answered` (`AGENT/src/run/batch.rs:60-71`). The slots live in private state (`batch.rs:8-36`; `RunState::ExecutingTools(ToolBatch, TurnPolicy)`, `mod.rs:280-283`). No accessor returns an answered slot.

### 1b. Building the part outside the run

- `ExecCall::answer(self, result: ToolResult) -> ToolAnswer` (`AGENT/src/run/calls.rs:50-59`) moves the `ToolResult` into a `ToolAnswer` whose fields are `pub(crate)` (`calls.rs:151-158`). Once answered, the driver cannot read it back.
- The run refuses result content built by a driver: the `compile_fail` doc test `run.tool_results(results)` (`calls.rs:140-150`).
- `tool_result_output` is public: `rig_core::tool::feedback::tool_result_output(call: CallId, name: ToolName, result: &ToolResult) -> UserContent` (`CORE/src/tool/feedback.rs:17-26`). It builds `ToolResult { call, name, content: result.output().clone().into_content(), is_error: !result.is_success() }`.
- `rig_core::message::ToolCall::answer(&self, result: &tool::ToolResult) -> message::ToolResult` (`CORE/src/completion/message.rs:521-528`) has the same body, without the `UserContent` wrapper.
- The call the run uses is the slot's `tool_call` (`mod.rs:1397-1400`). `ExecCall` is built from that same slot (`batch.rs:40-58`), and `ExecCall::tool_call()` exposes it (`calls.rs:40-43`). So `exec.tool_call().answer(&result)` equals the part Rig later commits for that call.
- `rig_core::tool::ToolResult` is `Clone` (`CORE/src/tool/result.rs:407-409`), so a driver can keep a copy before `exec.answer(result)`.
- Malformed calls: `malformed_answer` is private (`mod.rs:1780-1809`). With `None` (what keelrs sends, `keelrs/src/agent/rig/call.rs:26`) it builds `tool_result_message(id, name, invalid_arguments_feedback(name.as_str(), raw))` (`mod.rs:1792-1793, 1808`). Both functions are public (`feedback.rs:28-49`), and `MalformedCall` exposes `id()`, `name()` and `raw_arguments()` (`calls.rs:90-108`). Rebuilding this outside the run copies a private rule.

### 1c. `messages()` vs `full_history()`

- `messages()` returns `&[Arc<Message>]`, a borrow of the run's append-only log, without the input history. "While tools run, the last turn's calls are still unanswered here: this is the view to display, and `full_history` the one to resume from" (`mod.rs:628-634`).
- `full_history()` returns an owned `Vec<Message>`: input history plus `messages()`, each cloned, then `closure()` (`mod.rs:729-746`).
- `closure()` is `batch.interrupt()` while the run is `ExecutingTools` (`mod.rs:748-756`). `interrupt` lays out the answered parts and closes every unanswered call with `pairing::close_pending` (`batch.rs:73-93`). So mid-batch the last message holds real parts for answered calls and synthetic "closed" parts for the rest.
- Each call clones every message of the conversation (`mod.rs:740-745`), so finding one result this way costs O(history) per result.
- `canonical_history()` is `full_history()` passed through `pairing::repair` (`mod.rs:774-780`); it is for cancellation, not for reading results.

### 1d. The projection cursor

- `CommittedLog` is append-only; "the single in-place edit is the not-yet-sent prompt" (`AGENT/src/run/committed.rs:10-33`).
- `pub fn project(committed: &[Arc<Message>]) -> impl Iterator<Item = CommittedItem<'_>>` yields every tool call and tool result in commit order (`committed.rs:43-90`). Its doc gives the protocol: "A driver keeps a cursor, starting at `AgentRun::projection_start`, and after each protocol call projects the messages past it and moves it to the end" (`committed.rs:56-59`).
- `projection_start()` is the end of `messages()`, or, for a run resumed mid-batch, the assistant message that holds the open calls, "so a resumed run announces the calls whose results it streams" (`mod.rs:636-644`).
- No per-result view exists in this protocol: a partial batch is not in `messages()`, so `project` sees its results only once the batch commits.

### 1e. Hooks

`AgentRun` has none (`mod.rs:3-5`). The per-tool hooks belong to `AgentRunner` (section 2).

## 2. Rig's own `AgentRun` driver: `AgentRunner`

- The engine keeps a cursor exactly as the doc says: `let mut cursor = run.projection_start();` and a `project!` macro that projects `run.messages()[cursor..]` and sets `cursor = run.messages().len()` (`AGENT/src/agent/engine.rs:196-212`). It projects after a model turn (`engine.rs:380-381`) and after a tool batch (`engine.rs:405-419`).
- `drive_tool_calls` runs the calls with `buffer_unordered(concurrency)` and collects every answer in call order. "Nothing is committed until the whole batch settles" (`engine.rs:513-583`). Then it calls `run.answer_all(answers)` once (`engine.rs:585-602`).
- It does not read the results back. It reads only the length of the committed batch message to size its `executed` slots (`engine.rs:612-625`).
- So the stream items for tool results arrive after the batch commits. The `rig-agent-loop` note already records this ("`ToolResult` (after the batch settles)", `rig-agent-loop.md` section 3).
- Live per-call data comes from hooks: `on_dispatch` before a tool runs (can deny, patch or skip) and `on_outcome` after it resolves (`AGENT/src/agent/hook.rs:1075-1100`). `run_single_tool` calls them through `dispatch_tool_call` (`engine.rs:1319-1343`). keelrs does not use `AgentRunner` (it drives `AgentRun` itself, `RUN:1-2`), so these hooks never fire for keelrs.
- Rig's examples that persist a run mid-batch serialize the whole `AgentRun` (`../rig/examples/agent_with_durable_approval/src/main.rs:182-203`; `../rig/examples/agent_run_stepping/src/main.rs:139-162`). Partial answers survive in `RunState::ExecutingTools` (`mod.rs:280-283`). None of them stores single results.

## 3. Rig's coding agent: `rig-ecs` and `rig-harness`

- **No `AgentRun`.** `rig-ecs` depends on `rig-core` and `rig-memory` (`ECS/Cargo.toml:30,33`); `rig-harness` on `rig-core`, `rig-ecs`, `rig-memory`, `rig-tools` (`../rig/crates/rig-harness/Cargo.toml:33-43`). No source file of either crate names `AgentRun`. The `rig-agent-loop` note found the same at 0.45 (`rig-agent-loop.md` section 0).
- **The `rig.call` mark.** `const CALL_MARK: &str = "rig.call";` "marks a call that may change something from its start, and any call once it settled, until its reply's results are in" (`ECS/src/restore.rs:72-75`). Its value (`restore.rs:77-100`):
  - `Started { call: CallId, at: Option<CallRef> }`.
  - `Settled { call, at, result: ToolResult, facts: ToolResultContext }`: "The call settled with `result` ... while its reply still ran."
- The marks are asides on the agent's `Conversation`, stored apart from its messages (`rig_core::conversation::Aside`; `CORE` doc at `../rig/crates/rig-core/src/conversation/aside.rs:16`).
- **`Started`.** Before a call that may change something starts, `mark_started` writes the mark and the session log is flushed; if the flush fails, the call does not start (`ECS/src/stage.rs:1192-1220`, `restore.rs:146-168`). The `tool-call-ordering` note covers this and the `Reads` exception (section 3c).
- **`Settled`.** `commit_replies` runs in `Stage::Commit`. While any call of the running reply is unsettled, it writes a `Settled` mark for each settled call, with `outcome.tool_result()` as the result (`ECS/src/turn.rs:1847-1876`). `mark_settled` replaces that call's `Started` mark, or adds one, and leaves an existing `Settled` mark alone (`restore.rs:170-204`).
- **Commit.** Once every call settled, it builds the results with `reply_results` and calls `commit_results` (`turn.rs:1877-1881`). `reply_results` builds each part with `call.request.call.answer(&result)` from the call and its outcome, in reply order (`turn.rs:889-923`). `commit_results` pushes the one entry, then removes every mark no pending call still owes (`restore.rs:206-223`).
- **Restart.** A pending call with a `Settled` mark is answered with the stored result; one with a `Started` mark is answered "may have partly run" (`restore.rs:225-236`; `tool-call-ordering.md` section 3c).
- So the kernel never looks for a result inside the conversation. It records the result it has, per call, beside the conversation, and drops the record once the batch commits.

## 4. Flue: `tool_outcome` back into the conversation

- **One record per finished call.** On the agent loop's `tool_execution_end` event, Flue writes a `tool_outcome` record (`FLUE/session.ts:2590-2632`). It takes the result from the event (`event.result`, `session.ts:2607`), not from the message list. The record holds `assistantMessageId`, `toolCallId`, `toolName`, `isError`, `content`, an optional `output` and `terminate`, and `durationMs` (`FLUE/conversation-records.ts:232-263`). Its id is `record_tool_outcome_{assistantMessageId}_{toolCallId}` (`session.ts:2601, 2617`).
- **The reducer only indexes it.** `case 'tool_outcome'` checks the assistant message and the call exist and match by id and name, refuses a duplicate, and sets `conversation.toolOutcomes[key] = record.id` (`FLUE/conversation-reducer.ts:671-692`). No message is added.
- **One record per batch.** On `turn_end` with tool results, Flue looks up each outcome id by `(assistantMessageId, toolCallId)` and writes a `tool_results_committed` record with `assistantMessageId`, `parentId` and `outcomeIds` (`session.ts:2667-2715`; type at `conversation-records.ts:265-270`).
- **The commit builds the messages.** `case 'tool_results_committed'` checks the outcome ids cover every call exactly once, in the assistant's call order, then appends one `toolResult` entry per outcome, built by `toolResultMessage(outcome)` (`conversation-reducer.ts:693-754`, `1470-1489`). It then drops the outcome from the pre-commit index (`conversation-reducer.ts:755-770`).
- **What the model sees.** `buildConversationContext` walks the active path (`conversation-reducer.ts:1134-1139`). An assistant message with tool calls goes to the model only with a complete batch of results; an incomplete batch is skipped (`conversation-reducer.ts:1185-1205`).
- **Cursor.** Flue tracks the batch by `canonicalToolRequestMessageId`, set to the assistant message that asked, and cleared after the commit (`session.ts:2597-2600, 2721`). It is an id, not an index into the message list.

## 5. Side by side

| | Rig `AgentRunner` | Rig `rig-ecs` | Flue | keelrs today |
|---|---|---|---|---|
| Drives `AgentRun` | yes | no | no (its own loop) | yes |
| Per-result record | none (hooks only) | `rig.call` `Settled` aside with the `ToolResult` | `tool_outcome` record | `Event::ToolResult(part)` |
| Where the per-result data comes from | n/a | the call's outcome | the loop's end event | `run.full_history()` searched by call id (`RUN:224-239`) |
| Batch commit | one `answer_all` | one entry (`commit_results`) | `tool_results_committed` with outcome ids | Rig commits; keelrs moves its cursor (`RUN:142-144`) |
| How it tracks what is stored | index cursor from `projection_start` | marks removed on commit | `canonicalToolRequestMessageId` | `produced` index (`RUN:76-77, 213-221`) |

## 6. What this means for keelrs (inference)

1. **`result_of` can be replaced by building the part before `answer`.** For an executed call, `exec.tool_call().answer(&result)` (`CORE/src/completion/message.rs:521-528`) is the same value `answer_all` commits (`AGENT/src/run/mod.rs:1397-1401`). `call.rs` already has `exec` and the `ToolResult` in hand (`keelrs/src/agent/rig/call.rs:33-35`), so the running future could yield `(ToolAnswer, ToolResultPart)` instead of `(CallId, ToolAnswer)`. That removes `full_history()` and its O(history) copy per result, and the search by id. This is what `rig-ecs` (`turn.rs:919`) and Flue (`session.ts:2607`) do: keep the result they have, never read it back.
2. **The malformed call is the catch.** Its part comes from Rig's private `malformed_answer` (`mod.rs:1780-1809`). keelrs can rebuild it from public parts (`tool_result_message` + `invalid_arguments_feedback`, `MalformedCall::raw_arguments`), but then it copies a Rig rule that may change. Two other options: read only that case from `full_history()`, or keep `result_of` for it. `rig-ecs` avoids the question because it never uses `AgentRun`.
3. **The `produced` cursor matches Rig's documented protocol.** `project` / `projection_start` describe exactly a cursor moved to the end after each protocol call (`committed.rs:56-59`), and Rig's engine resets it the same way after a batch (`engine.rs:211`). keelrs's hand reset after a batch (`RUN:142-144`) is the same move. Two details differ: keelrs starts at `0` (`RUN:114`), Rig at `projection_start()`. The two agree for a fresh run. They differ only for a run resumed mid-batch, which keelrs does not do: it rebuilds a fresh `AgentRun` from history (`RUN:88-106`).
4. **No `AgentRun` API removes the reset.** A batch cannot be committed result by result (`mod.rs:1438-1444`; `calls.rs:140-150`). So a driver that stores results one by one must skip the batch message Rig commits later. keelrs does this by moving `produced` to the end. `rig-ecs` does it by removing the marks at commit. Flue does it with a separate commit record.
5. **keelrs has no batch-commit marker.** Its history fold groups `Event::ToolResult` parts after the last assistant message in call order (`keelrs/src/agent/rig/history.rs:35-80`), close to Flue's commit step. Both Rig's kernel and Flue mark the commit explicitly, and Flue's context builder drops an incomplete batch (`conversation-reducer.ts:1195-1204`). Whether keelrs needs such a marker for recovery is outside this ticket; the durable-agent work would decide it.
