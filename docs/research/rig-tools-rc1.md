# Rig tools on 0.46.0-rc.1, and how `AgentRun` hands tool calls to a driver

Research for issue #22 (map: #7). This rechecks [`rig-tools.md`](rig-tools.md), which was written against Rig 0.45.0, against `0.46.0-rc.1`, the version keelrs pins. It records only what differs or is new. The Flue side was not rechecked. Section 3 sketches options for keelrs's driver; it makes no decisions.

**Sources.**

- Rig (`0xPlaygrounds/rig`) at tag [`v0.46.0-rc.1`](https://github.com/0xPlaygrounds/rig/tree/v0.46.0-rc.1), commit `6eff50a2` ("chore: release v0.46.0-rc.1", 2026-10-23). It is the commit recorded in the published crates' `.cargo_vcs_info.json`. The `rig-agent/src` and `rig-core/src/tool` trees in `~/.cargo/registry` are byte-identical to the tag (`diff -r`). In the links below, `RC` stands for `https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates`.
- The diff [`v0.45.0...v0.46.0-rc.1`](https://github.com/0xPlaygrounds/rig/compare/v0.45.0...v0.46.0-rc.1), which is 6 commits.
- API docs: [docs.rs/rig-agent/0.46.0-rc.1 `run`](https://docs.rs/rig-agent/0.46.0-rc.1/rig_agent/run/index.html) and [docs.rs/rig-core/0.46.0-rc.1 `tool`](https://docs.rs/rig-core/0.46.0-rc.1/rig_core/tool/index.html).
- keelrs's driver, [`keelrs/src/agent/rig.rs`](https://github.com/fpaschos/keelrs/blob/46d74e566e110eaf9e7df9f7f6aafef449949e30/keelrs/src/agent/rig.rs) at `46d74e5`.

## Summary

- **The tool API did not change between 0.45.0 and 0.46.0-rc.1.** `git diff v0.45.0 v0.46.0-rc.1` is empty for `crates/rig-agent/src`, `crates/rig-core/src/tool` and `crates/rig-derive`. Every finding in `rig-tools.md` §1 holds on rc.1 as written: `Tool`, `DynamicTool`, `ToolDefinition`, `ToolSet`, `args_schema`/schemars, `tool::validate`, `ToolExecutionError`, concurrency, and duplicate names. Its `v0.45.0` links still point at the same code.
- **What rc.1 does change is outside the tool API**: catalog data and input-limit fitting, `ContextUse.window` renamed to `.limit`, a new 300 s idle timeout on streamed replies (`ProviderError::Stalled`), and one provider encoder's handling of error tool results (§1).
- **`AgentRun` is a sans-I/O state machine. It owns no tools.** The driver passes the turn's `ToolDefinition`s to `prepare_request`, which returns a `TurnPolicy` naming the callable tools. The driver sends the request and feeds the reply back with `ModelTurn::from_policy(.., policy)`. The run then checks each call's name against that policy and stops at the first call it does not allow with `ModelTurnOutcome::NeedsResolution`. Once the names check out, `next_step` returns `AgentRunStep::CallTools { calls }`. Each call is `Execute` (run it) or `Malformed` (the arguments are not a JSON object, so don't run it). The driver answers every call through the call's own `answer(..)`. The run then commits all the results as one user message in call order and moves on to the next `CallModel`.
- **keelrs today can never reach `CallTools`.** It passes `Vec::new()` as the tool list, so the policy allows no names. Any call the model makes becomes `NeedsResolution(UnknownTool)`. `AgentRun::new` also defaults to `max_turns(1)`, so even after tools were answered, the follow-up model call would fail with `PromptError::MaxTurns`. A tool loop needs both a tool list and a larger turn budget.
- **What the run checks for each call**: the name (against the turn's policy), whether the arguments are a JSON object, an output-tool interception, and a failed or truncated turn. It does **not** validate arguments against the tool's JSON schema. A driver that wants that check can call `rig_core::tool::feedback::arguments_refusal(parameters, call)` before running the tool.

## 1. What differs on rc.1 (tools and adjacent)

The six commits after 0.45.0 are listed in the [compare view](https://github.com/0xPlaygrounds/rig/compare/v0.45.0...v0.46.0-rc.1). Two touch `rig-core` (`12256a37`, catalog and compaction; `049b4a9b`, provider handling). The others touch the harness, plugin and `rig-tools` crates, which keelrs does not use. The `CHANGELOG.md` at the tag has no 0.46 section yet. Its only change is a rewritten 0.45.0 section.

| Area | Change on rc.1 | Source |
| --- | --- | --- |
| Tool API (`rig-core::tool`, `rig-agent`, `rig-derive`) | None | empty `git diff v0.45.0 v0.46.0-rc.1 -- crates/rig-agent/src crates/rig-core/src/tool crates/rig-derive` |
| `ContextUse` | Field `window: Option<u32>` renamed to `limit` | [`RC/rig-core/src/completion/request.rs#L1108-L1113`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/completion/request.rs#L1108-L1113) |
| Streamed model calls | Fail with `ProviderError::Stalled(d)` after `STREAM_IDLE_TIMEOUT` (300 s) with no data. The error is a retryable timeout. `Model::with_idle_timeout(None)` turns it off. Unary calls are not affected. | [`RC/rig-core/src/driver.rs#L71`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/driver.rs#L71), [`#L96`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/driver.rs#L96), [`RC/rig-core/src/error.rs#L364`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/error.rs#L364) |
| One provider's encoder (commit `049b4a9b`) | Error tool results: images move after the result run, blank text parts are dropped, and an empty error result is sent as `"Tool failed with no output."`. This changes the wire format only. The `ToolResult` API is the same. | compare view, `crates/rig-core/src/providers/*/completion.rs` |
| Catalog | Model data refreshed, input-limit fitting, compaction overflow recovery | commit `12256a37` |

Because the code is the same, nothing in `rig-tools.md` §3 (side by side) or §4 (options for `use_tool`) needs to change for rc.1.

One detail of the unchanged `ToolContext` that `rig-tools.md` did not mention: besides its serialised slots, it carries non-serialised `scope`s (`with_scope(Arc<dyn Any + Send + Sync>)`, `scope::<T>()`). A host can pass live handles to a tool through it ([`RC/rig-core/src/tool/context.rs#L209-L230`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/tool/context.rs#L209-L230)).

## 2. How `AgentRun` hands tool calls to a driver (rc.1)

The module doc: "Drivers act on `AgentRunStep` and supply responses or tool results before advancing. A run owns no models, tools, or hooks" ([`RC/rig-agent/src/run/mod.rs#L1-L12`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1-L12)). Rig's own agent loop (`agent/engine.rs`) is one such driver. It is the reference for everything below.

### 2a. The steps

```rust
pub enum AgentRunStep {
    CallModel { prompt: Arc<Message>, history: Vec<Arc<Message>>, turn: usize },
    CallTools { calls: Vec<PendingToolCall> },   // unanswered calls, emission order
    Done(PromptResponse),
}
pub enum ModelTurnOutcome {
    Continue { response_hook_suppressed: bool },
    NeedsResolution(InvalidToolCallContext),     // unknown or disallowed tool name
    TurnRetried,                                 // rolled back with feedback; call next_step
}
```

([`run/mod.rs#L76-L101`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L76-L101), [`#L191-L212`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L191-L212))

The run's internal states follow one path: `PreparingRequest` → `AwaitingModel` → `ResolvingToolCalls` → `AwaitingAdvance` → `ExecutingTools` → back to `PreparingRequest`, or on to `Done` / `Failed` ([`#L264-L289`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L264-L288)).

### 2b. How tool definitions reach the request and the run

`prepare_request(spec, capabilities, history, tools: Vec<Arc<ToolDefinition>>, committed_output_tool, patch)` ([`run/prepare.rs#L121-L315`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/prepare.rs#L121-L315)):

- It narrows `tools` by the patch's `active_tools`, if any. An allow-list name that is not in `tools` is a `PrepareError`. The remaining names become the turn's **executable** set.
- In Tool output mode it appends the synthetic output tool (only when an `output_schema` is set; keelrs sets none).
- It builds `TurnPolicy::resolve(executable, tool_choice, output_tool, ..)`. This also rejects impossible choices before any I/O, such as `Required` with no tools.
- It returns a `PreparedRequest` whose `tools` and `policy` are public fields. `apply(CompletionRequest)` writes the tools and the tool choice onto the request ([`#L97-L113`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/prepare.rs#L97-L113)).

The run never sees the definitions themselves. It sees only the `TurnPolicy`, which the driver hands back with the reply: `ModelTurn::from_policy(&response, prepared.policy)` ([`run/mod.rs#L134-L149`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L134-L149)). `TurnPolicy::allows(name)` is "the executable tools narrowed by the tool choice, plus the output tool" ([`run/policy.rs#L209-L239`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/policy.rs#L209-L239)).

There is an optional extra step. `run.advertise_tools(turn, definitions)` stores the definitions sent for a turn as run data, "so a second driver (or a resumed run) can re-pair tool calls with what was offered". `advertised_tools()` reads them back. The run does not validate calls against this record; it is there for the driver ([`run/mod.rs#L372-L388`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L372-L388), [`#L472-L483`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L472-L483)). Rig's loop calls it right after preparing each request ([`agent/engine.rs#L349`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/engine.rs#L349)). Its own preparation snapshots the registry, passes the snapshot's definitions to `prepare_request`, then keeps only `prepared.policy.executable()` in the snapshot. This way the implementation that runs is the one whose definition was sent ([`agent/completion.rs#L95-L117`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/completion.rs#L95-L117)). The loop holds that snapshot until the immediately following `CallTools` step ([`engine.rs#L190-L193`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/engine.rs#L190-L193), [`#L382-L405`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/engine.rs#L382-L405)).

### 2c. Name validation: `NeedsResolution`

`model_response(turn)` records the completion call and its usage, then scans the reply's tool calls in order. It stops at the first call whose name the policy does not allow and returns `NeedsResolution(InvalidToolCallContext)` ([`run/mod.rs#L1049-L1081`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1049-L1081), [`#L1463-L1515`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1463-L1515)). The context carries the tool name, call id, raw args, available and allowed tools, tool choice, a diagnostic history, and a `reason`: `UnknownTool` or `DisallowedByToolChoice` ([`run/policy.rs#L24-L91`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/policy.rs#L24-L91)).

The driver must answer before calling `next_step` again; calling it first is a protocol violation ([`run/mod.rs#L1032-L1037`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1032-L1037)). It has three ways to answer:

- `resolve_invalid_tool_call(InvalidToolCallAction)` ([`#L1257-L1324`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1257-L1324)):
  - `Fail` fails the run with `PromptError::UnknownToolCall`.
  - `Retry { feedback }` commits the rejected turn and the feedback, then returns `TurnRetried`. It needs `max_invalid_tool_call_retries > 0`, which defaults to 0, and it also uses up the turn budget.
  - `Repair { tool_name }` renames the call; the new name is revalidated.
  - `Skip { reason }` gives the call a synthetic result, and **every other call in the turn** gets "not executed".
  - `Stop { reason }` cancels the run.
- `resolve_unhandled_invalid_tool_call()` applies `RunSpec::unhandled_invalid_tool_call`: `Fail` (the default) or `Ignore`, which drops the call ([`#L1329-L1357`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1329-L1357), [`run/spec.rs#L74-L87`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/spec.rs#L74-L87)).
- `ignore_invalid_tool_call()` drops the call regardless of the spec's policy.

Each answer returns another `ModelTurnOutcome`, because the scan continues to the next invalid call. Rig's loop asks its `on_invalid_tool_call` hook in a loop, falls back to `resolve_unhandled_invalid_tool_call` when the hook returns `None`, and stops at `Continue` or `TurnRetried` ([`engine.rs#L1492-L1515`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/engine.rs#L1492-L1515)).

### 2d. `next_step` after an accepted turn

From `AwaitingAdvance`, `next_step` ([`run/mod.rs#L873-L1021`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L873-L1021)) does the following, in order:

1. **Failed or truncated turn.** If `turn_failure(..)` reports one, the turn's calls never run, and the run returns `ProviderError::Response` ([`#L885-L895`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L885-L895)).
2. **Output tool.** If the policy names an output tool and the model called it, the call's arguments become the final answer, and its sibling calls do not run.
3. **Classify each call into a slot** ([`run/batch.rs#L23-L29`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/batch.rs#L23-L29)). A call already answered (skipped while resolving) is `Answered`. A call whose arguments are not a JSON object (`function.invalid_arguments.is_some()`) is `Malformed`. Anything else is `Execute`.
4. **Count malformed steps.** Consecutive steps containing a malformed call are counted against `max_consecutive_malformed_tool_calls` (default `None`, no limit) ([`#L1143-L1171`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1143-L1171)).
5. **Commit and hand over.** The assistant turn is committed and the state becomes `ExecutingTools`, which emits `CallTools { calls: batch.pending(turn) }` ([`#L1022-L1026`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1022-L1026)). A turn with no tool calls finishes instead, as `Done`.

### 2e. Executing and feeding results back

`PendingToolCall` ([`run/calls.rs#L11-L126`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/calls.rs#L11-L126)):

- **`Execute(ExecCall)`**: `id()`, `name()`, `tool_call()` and `arguments() -> serde_json::Value` (always a JSON object). Answer with `call.answer(ToolResult) -> ToolAnswer`.
- **`Malformed(MalformedCall)`**: `id()`, `name()` and `raw_arguments()`. The call cannot be executed or answered as executed, and doc tests enforce both. Answer with `call.answer(Option<InvalidToolCallAction>)`:
  - `None` or `Retry` answers with feedback asking the model to call again ("The arguments for tool `x` are not a JSON object: … Call the tool again with a JSON object as its arguments.").
  - `Skip` gives a skipped result.
  - `Stop`, `Fail` and `Repair` end the run.

  `run.malformed_context(&call, is_streaming)` builds the `InvalidToolCallContext` to show a hook ([`run/mod.rs#L1175-L1196`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1175-L1196), [`#L1782-L1809`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1782-L1809)).

`ToolAnswer` has private fields: the turn, the slot index and the call id. Only the pending call can build one. Answers go back through `run.answer(a)` or `run.answer_all(iter)` ([`run/mod.rs#L1360-L1448`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1360-L1448)). The rules:

- **Any order.** Answers may arrive out of order and across several calls. Each answer is checked against its turn, index and id. An answer that is stale, already applied or mismatched is a protocol violation that stores nothing.
- **The run builds each result itself.** It turns a `ToolResult` into a `UserContent::ToolResult` with `is_error = !result.is_success()` ([`RC/rig-core/src/tool/feedback.rs#L19-L26`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/tool/feedback.rs#L19-L26)).
- **One commit per batch.** Once every slot is answered, the batch is committed as **one user message in call order**, and the state returns to `PreparingRequest`. Until then, `next_step` re-emits only the unanswered calls. "A persisted run resumes from here in any non-terminal state except a pending invalid tool-call resolution" ([`#L830-L836`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L830-L836)).
- **Persistence.** `AgentRun` is `Serialize + Deserialize` with envelope `RUN_FORMAT = 5`. Any other format is refused by name ([`#L356-L370`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L356-L370)).
- **Turn budget.** The follow-up model call needs budget. `AgentRun::new` sets `max_turns: 1` ([`#L393-L418`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L393-L418)). `next_step` returns `PromptError::MaxTurns` when `current_turn >= max_turns` ([`#L858-L868`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L858-L868)). Raise it with `.max_turns(n)`, or with `AgentRun::from_spec(&spec, ..)` and `spec.max_turns` ([`#L1725-L1752`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/run/mod.rs#L1725-L1752)).

Executing a call is entirely up to the driver. With a `rig_agent::tool::ToolSet`, the direct call is `set.execute(name, args_string, &mut ToolContext::default()).await -> ToolResult`. An unknown name returns a `not_found` failure with model feedback ([`RC/rig-agent/src/tool/registry.rs#L393-L411`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/tool/registry.rs#L393-L411)). A typed tool's adapter deserialises the arguments with serde and returns `InvalidArgs` on failure, as `rig-tools.md` §1c describes.

Rig's loop adds hooks (`on_dispatch`/`on_outcome`), handler layers and its own `buffer_unordered(concurrency)` ([`engine.rs#L482-L627`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/engine.rs#L482-L627), [`#L1585-L1700`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-agent/src/agent/engine.rs#L1585-L1700)). These belong to the loop. The run protocol does not need them.

### 2f. What `AgentRun` validates per call, and what it does not

| Check | Who | Outcome |
| --- | --- | --- |
| Name advertised and allowed by the tool choice | run (`model_response`) | `NeedsResolution`; the driver chooses Fail, Retry, Repair, Skip, Stop or Ignore |
| Arguments are a JSON object | run (batch classification) | `PendingToolCall::Malformed`, never executed; the default answer asks the model to retry |
| Consecutive malformed steps | run | Fails past `max_consecutive_malformed_tool_calls`, if set |
| Failed or truncated turn | run (`next_step`) | No call runs; `ProviderError::Response` |
| Arguments match the tool's JSON schema | **nobody** by default | The driver can call `feedback::arguments_refusal(&def.parameters, call.tool_call())` and answer `ToolResult::failed(ToolExecutionError::invalid_args(..).with_model_feedback(msg))` ([`RC/rig-core/src/tool/feedback.rs#L118-L175`](https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/rig-core/src/tool/feedback.rs#L118-L175)) |
| Arguments deserialise into a typed `Args` | the tool adapter (in `execute`) | `InvalidArgs` failure result; the run continues |
| Unknown name at execution (registry changed) | `ToolSet::execute` | `not_found` failure result |

## 3. What a keelrs tool loop would touch (options, not decisions)

Today's driver ([`rig.rs#L37`](https://github.com/fpaschos/keelrs/blob/46d74e566e110eaf9e7df9f7f6aafef449949e30/keelrs/src/agent/rig.rs#L37), [`#L63-L85`](https://github.com/fpaschos/keelrs/blob/46d74e566e110eaf9e7df9f7f6aafef449949e30/keelrs/src/agent/rig.rs#L63-L85)):

- It builds `AgentRun::new(..)`, so `max_turns` is 1.
- It passes `Vec::new()` as tools to `prepare_request`.
- It feeds `ModelTurn::from_policy(&response, policy)` back to the run.
- It errors on `NeedsResolution` and on `CallTools`.

The `Settled` / `Done` handling already copes with tool turns, because `PromptResponse::messages` includes the call and result messages.

The run protocol requires these changes, whatever keelrs designs on top:

1. **Turn budget.** Use `AgentRun::new(..).max_turns(n)` (or `from_spec`). Choosing `n` is a keelrs decision; with the default of 1 the run cannot reach a second model call.
2. **Tools per turn.** In `CallModel`, after `agent.declare()`, take the declared tools as `Vec<Arc<ToolDefinition>>` and pass them as `prepare_request`'s fourth argument. Keep the executable implementations for that turn alive until the following `CallTools`. Rig's loop pins its snapshot this way; with keelrs's re-render, the set built from that render is the natural thing to pin. Optionally call `run.advertise_tools(turn, prepared.tools.clone())` so a resumed run can re-pair calls with what was offered.
3. **`CallTools { calls }`.** For each `Execute`, look up the pinned implementation, run it, and `call.answer(result)`. For each `Malformed`, `call.answer(None)` gives Rig's default retry feedback. Then call `run.answer_all(answers)` and loop. The options:
   - **Sequential or concurrent.** Run the calls one after another, or concurrently with `join_all` / `buffer_unordered`. Rig sorts the results into call order either way.
   - **Answer per call or once per batch.** Answer each call as it settles, or once per batch. Answering per call gives the finest durable checkpoints (see the sqlx direction), and a resumed run re-emits only the unanswered calls.
   - **Schema check.** Optionally run `arguments_refusal` before executing, for Flue-style validation.
4. **`NeedsResolution(ctx)`.** This happens when the model names a tool that was not advertised this turn, for example one a re-render just removed. The options:
   - **Fail** (today's behaviour, via `resolve_unhandled_invalid_tool_call` with the default spec).
   - **Retry with feedback** (`resolve_invalid_tool_call(InvalidToolCallAction::retry(..))`, which needs `max_invalid_tool_call_retries > 0`).
   - **Skip** (note that this suppresses every sibling call in the turn).
   - **Ignore** (`ignore_invalid_tool_call` or `UnhandledInvalidToolCall::Ignore`).

   Keep calling the resolver until it returns `Continue` or `TurnRetried`.
5. **Where tool execution lives.** Choosing between `rig_agent::tool::ToolSet` + `DynamicTool` and a keelrs-owned name→callback map is the open question already listed in `rig-tools.md`. The run protocol works with either, because it only needs `ToolDefinition`s going in and `ToolResult`s coming back.

## Open questions

- The run protocol lets keelrs persist an `AgentRun` mid-batch (a `CallTools` with some calls answered) and resume it. Whether keelrs's durable receipts should store the serialised `AgentRun` (Rig's `RUN_FORMAT`) or keelrs's own records is not settled here.
- rc.1 is a prerelease. Re-diff `crates/rig-agent/src/run` and `crates/rig-core/src/tool` when 0.46.0 final ships.
