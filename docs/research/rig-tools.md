# How `use_tool` would work on Rig

Research for issue #11 (map: #7). Tools are not implemented in this map. Section 4 sketches options for keelrs; it makes no decisions.

**Sources.**

- Rig (`0xPlaygrounds/rig`) at the latest release, tag [`v0.45.0`](https://github.com/0xPlaygrounds/rig/tree/v0.45.0) (2026-10-19). crates.io lists `rig-core` and `rig-agent` 0.45.0 as the newest stable versions; `main` is at `0.46.0-rc.1`. In the links below, `RIG` stands for `https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates`. API docs: [docs.rs/rig-core/0.45.0](https://docs.rs/rig-core/0.45.0/rig_core/tool/) and [docs.rs/rig-agent/0.45.0](https://docs.rs/rig-agent/0.45.0/rig_agent/tool/).
- Flue (`withastro/flue`) at commit `deee93b` (2026-10-12). In the links below, `FLUE` stands for `https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc`. Runtime paths are under `packages/runtime/src/`; docs are under `apps/docs/src/content/docs/`.

## Summary

- **Rig has changed a lot since the API most write-ups describe.** `ToolDyn` is no longer public, `ToolSet` moved out of `rig-core` into the new `rig-agent` crate, and `Tool::call` now takes a `&mut ToolContext`. Runtime-defined tools are the concrete `DynamicTool` struct, which pairs a `ToolDefinition` with an `Arc` callback over `serde_json::Value`.
- **Typed input schemas use schemars.** `rig_core::tool::args_schema::<A: JsonSchema>()` produces the plain schema that tool definitions use. The `#[rig_tool]` macro generates the same thing from a function. At dispatch, Rig only deserialises the arguments with serde. A JSON-Schema validator (`tool::validate`) exists, but the agent loop does not call it.
- **The tool set can change on every turn.** Before each model call, Rig's agent loop takes a fresh snapshot of its `ToolServerHandle` registry. A completion-call hook runs before that snapshot and can return a per-turn `RequestPatch` with an `active_tools` allow-list. Each call runs on the registration that its turn pinned.
- **Parallel tool execution is opt-in.** `tool_concurrency(n)` runs calls through `buffer_unordered(n)`. The default is 1, which runs calls in order. Results are always committed to history in call order, and only once the whole batch settles. Flue does the opposite: it runs a batch in parallel by default.
- **Tool errors become error results that the model sees, in both frameworks.** Rig carries a structured `ToolExecutionError` with a kind, an optional `model_feedback` and a retryable flag. Flue sends a throw, a validation failure or a timeout back as the tool's error result.
- **Duplicate names behave differently.** Flue throws `ToolNameConflictError`, both at render time and when it assembles the tool set. Rig's `ToolSet` silently replaces the earlier tool, logging only a `tracing::warn!`. A keelrs `use_tool` would need its own duplicate check to match Flue.

## 1. Rig's tool model (v0.45.0)

### 1a. The `Tool` trait

`rig_core::tool::Tool` ([`RIG/rig-core/src/tool/contextual.rs#L32-L92`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/contextual.rs#L32-L92)):

```rust
pub trait Tool: Sized + WasmCompatSend + WasmCompatSync {
    const NAME: &'static str;
    type Args: for<'de> Deserialize<'de> + WasmCompatSend + WasmCompatSync;
    type Output: IntoToolOutput;
    type Error: std::error::Error + WasmCompatSend + WasmCompatSync + 'static;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;
    fn annotations(&self) -> ToolAnnotations { .. }          // read-only, destructive, ...
    fn footprint(&self, args: &Self::Args) -> Touches { .. } // what a call reads and writes
    fn map_error(&self, error: Self::Error) -> ToolExecutionError { .. }
    fn call(&self, context: &mut ToolContext, args: Self::Args)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + WasmCompatSend;
}
```

Things to note:

- **The name is a `const`.** A typed tool cannot take its name from a runtime value. `tool_name::<T>()` turns an empty `NAME` into a build error ([`contextual.rs#L382-L385`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/contextual.rs#L382-L385)).
- **`parameters()` is written by hand**, and nothing ties it to `Args`. The helper in 1c closes that gap.
- **Only one `call` signature.** The CHANGELOG entry for the 0.4x tool rework says typed tools now implement only `Tool::call(&mut ToolContext, Args)`, `ToolDyn` is removed from the public API, and the erased dispatch trait is private ([`CHANGELOG.md#L769-L778`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/CHANGELOG.md#L769-L778); migration table in [`MIGRATING.md#L4170-L4211`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/MIGRATING.md#L4170-L4211)).
- **`ToolContext` is per-call data, not shared state.** It holds typed inbound values (`insert`, `require`) and host-only result metadata (`insert_result`). Every value is serialised, and its docs say that values with interior sharing "belong to the tool instance" ([`RIG/rig-core/src/tool/context.rs#L21-L48`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/context.rs#L21-L48)).

### 1b. Dynamic tools: `DynamicTool`

`DynamicTool` is the runtime-defined tool: a `ToolDefinition` (name, description, JSON schema, annotations) plus `Arc<dyn ToolCallback>`. It is `Clone` ([`contextual.rs#L141-L210`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/contextual.rs#L141-L210)).

- `DynamicTool::new(name: ToolName, description, parameters: Value, callback)` takes a callback `Fn(Value) -> WasmBoxedFuture<'static, Result<ToolOutput, ToolExecutionError>> + Send + Sync + 'static`.
- `new_with_context` passes the callback the `&mut ToolContext` as well.
- `run(args)` / `run_with(&mut ctx, args)` call the tool directly, with no registry or agent involved. That is useful in tests.
- `with_liveness(..)` takes a probe that registries use to drop tools from a source that has gone away, such as MCP ([`RIG/rig-core/src/tool/managed.rs`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/managed.rs)).

The callback is `'static` and owns its captures. A closure over render-time values therefore has to clone or `Arc` them in, then clone again into each `async move` future.

### 1c. Typed input schemas (schemars)

`rig-core` depends on `schemars` ([`RIG/rig-core/Cargo.toml#L41`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/Cargo.toml#L41)). `args_schema::<A: JsonSchema>()` derives a tool's parameter schema from its argument type, "so the schema the model sees and the type its arguments are parsed into cannot disagree" ([`RIG/rig-core/src/tool/schema.rs#L1-L45`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/schema.rs#L1-L45)). It uses draft 2020-12 and inlines subschemas. It drops `$schema`, `title`, `default` and `format`, makes `Option` fields optional rather than nullable, and adds `additionalProperties: false`. Field doc comments become `description`s.

`#[rig_tool]` (`rig-derive`) turns a `fn` that returns `Result<T, E>` into a `Tool` type plus an argument struct with schemars attributes. The function may take `&mut ToolContext` ([`RIG/rig-derive/src/lib.rs#L50-L91`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-derive/src/lib.rs#L50-L91)).

**Validation at dispatch.** The typed-tool adapter deserialises the argument string with `serde_json::from_str`, treating a literal `null` as `{}`. A failure becomes `ToolExecutionError::invalid_args(..)` ([`contextual.rs#L110-L125`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/contextual.rs#L110-L125)). Rig also has a JSON-Schema subset validator, `tool::validate::validate(schema, value)`, and a helper built on it, `tool::feedback::arguments_refusal`, which produces model feedback ([`RIG/rig-core/src/tool/validate.rs#L1-L40`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/validate.rs#L1-L40), [`feedback.rs#L131`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/feedback.rs#L131)). In this release only `rig-ecs` calls it (`rig-ecs/src/tools.rs:462`), not the `rig-agent` loop. Separately, the loop treats arguments that are not a JSON object as a malformed call, and `max_consecutive_malformed_tool_calls` caps how many turns in a row may do that ([`RIG/rig-agent/src/agent/runner.rs#L436-L446`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/runner.rs#L436-L446)).

### 1d. `ToolSet` and `ToolServerHandle` (now in `rig-agent`)

- **`ToolSet`** is an `IndexMap` of named registrations, each flagged as always advertised or retrieval-only. It is `Clone + Default`, with `add_tool`, `add_dynamic_tool`, `add_retrieved_tool`, `remove_tool`, `tool_definitions` and `execute(name, args, &mut ToolContext) -> ToolResult` ([`RIG/rig-agent/src/tool/registry.rs#L243-L412`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/tool/registry.rs#L243-L412)). Calling `execute` with an unknown name returns a `not_found` failure that carries model feedback ([`registry.rs#L396-L411`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/tool/registry.rs#L396-L411)).
- **Duplicate names: last one wins.** `insert_registration` replaces the existing entry and only logs `tracing::warn!(.. "replacing an existing tool registration")` ([`registry.rs#L300-L309`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/tool/registry.rs#L300-L309)).
- **`ToolServer` / `ToolServerHandle`** is the mutable, shared registry behind an `Agent` (an `Arc<RwLock<..>>`). Its `add_tool`, `add_dynamic_tool`, `add_tools` and `remove_tool` methods are synchronous, and `snapshot()` returns a `ToolCatalog` ([`RIG/rig-agent/src/tool/server.rs#L284-L475`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/tool/server.rs#L284-L475)). Its module docs say that "a request catalog keeps the registration it pinned and runs it directly" after a replacement ([`server.rs#L1-L13`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/tool/server.rs#L1-L13)).
- **`AgentBuilder` registration:** `.tool(T)`, `.dynamic_tool(DynamicTool)`, `.dynamic_tools(Vec<_>)` and `.tool_server_handle(handle)` ([`RIG/rig-agent/src/agent/builder.rs#L569-L600`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/builder.rs#L569-L600)).
- **Underneath the agent loop** is the plain request type, `rig_core::completion::CompletionRequest`. It has a `tools: Vec<Arc<ToolDefinition>>` field with `.tool(..)` / `.tools(..)` builders ([`RIG/rig-core/src/completion/request.rs#L1219`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/completion/request.rs#L1219), [`#L1549-L1556`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/completion/request.rs#L1549-L1556)). A host that runs its own loop can therefore send any tool list it likes on each call, then dispatch the calls with `ToolSet::execute`.

### 1e. Changing the tool set per turn

Each model call within one multi-turn run is prepared like this ([`RIG/rig-agent/src/agent/engine.rs#L283`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/engine.rs#L283) then [`#L331`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/engine.rs#L331)):

1. **`AgentHook::on_completion_call`** fires with `CompletionCallEvent { prompt, history, turn }`. It returns `Continue`, `Stop(reason)` or `Patch(RequestPatch)` ([`RIG/rig-agent/src/agent/hook.rs#L337-L344`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/hook.rs#L337-L344), [`#L880-L888`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/hook.rs#L880-L888), [`#L983-L989`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/hook.rs#L983-L989)). A `RequestPatch` can replace the `preamble` and set `tool_choice`, `active_tools` (an allow-list), `extra_context` and `history` for **this turn only**. It "is not carried into subsequent turns" ([`RIG/rig-agent/src/run/patch.rs#L25-L47`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/run/patch.rs#L25-L47)).
2. **`build_prepared_completion_request`** then takes `tool_server_handle.snapshot_with_dynamic(..)` and narrows the snapshot to the tools that were actually advertised, "so the implementation behind every definition the provider received is the one that runs" ([`RIG/rig-agent/src/agent/completion.rs#L95-L117`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/completion.rs#L95-L117)).
3. **Tool calls from that turn run against the pinned snapshot**: "The call runs on the registration the catalog pinned for this turn" ([`engine.rs#L1623`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/engine.rs#L1623)).

So there are two ways to change the tool set per turn inside Rig's own loop. A hook can mutate a shared `ToolServerHandle` in `on_completion_call`, because the snapshot is taken after the hook runs. Or a hook can narrow a fixed set with `active_tools`. Outside the loop, a host can build each `CompletionRequest` itself.

### 1f. Parallel tool execution

`AgentRunner::tool_concurrency(n)` "Defaults to one; zero is clamped to one" ([`runner.rs#L418-L429`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/runner.rs#L418-L429)). `drive_tool_calls` runs the batch through `stream::iter(..).buffer_unordered(concurrency)`, with these rules ([`engine.rs#L455-L557`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/engine.rs#L455-L557)):

- Answers are collected and committed to history **in call order, only after the whole batch settles**.
- After the first hook termination or fail-closed error, no new call starts. Calls already running are drained, and the error with the lowest call index is returned.
- Malformed calls never run. `on_invalid_tool_call` decides what the model is told.

`Tool::footprint` / `Touches` lets a tool declare what it reads and writes, "so a host runs the calls of one reply that touch different things side by side" ([`contextual.rs#L64-L74`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/contextual.rs#L64-L74)). The `rig-agent` loop above does not consult it. `rig-ecs` and `rig-tools` (the harness crates) do.

### 1g. Tool errors

`ToolExecutionError` has a `ToolErrorKind`: `InvalidArgs`, `Timeout`, `Cancelled`, `NotFound`, `PermissionDenied`, `RateLimited`, `Provider`, `Network` or `Other`. Each kind has a default retryability and default model feedback. The error can also carry `with_model_feedback`, `with_model_output`, `with_retryable`, `with_code` and a source error, and `refused(..)` marks a deliberate refusal ([`RIG/rig-core/src/tool/result.rs#L20-L300`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/result.rs#L20-L300)). A typed tool's `Error` is converted by `map_error` only at the erased dispatch boundary.

The result of a call is a `ToolResult`: success, failed or skipped. Anything other than success goes to the model as a tool result with `is_error: true` ([`RIG/rig-core/src/tool/feedback.rs#L19-L37`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/tool/feedback.rs#L19-L37)), and the run continues. Only a hook's `Stop` or a fail-closed dispatch error ends the run. Hooks `on_dispatch` / `on_outcome` can rewrite a call's arguments, skip the call, or rewrite its result ([`hook.rs#L699-L840`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-agent/src/agent/hook.rs#L699-L840)).

## 2. Flue's `useTool` / `defineTool`

- **`defineTool(opts)`** validates the definition and returns it frozen. Its fields are `name`, `description`, `input?`, `output?`, `harness?`, `durable?`, `timeoutMs?`, `annotations?` and `run` ([`FLUE/packages/runtime/src/tool.ts#L21-L57`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/tool.ts#L21-L57)).
- **`assertToolDefinition`** rejects unknown keys (a typo like `harnes` must not silently produce a pure tool), blank names or descriptions, and an `input` that is not a top-level Valibot object schema. It also checks annotation fields ([`tool.ts#L79-L147`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/tool.ts#L79-L147)).
- **`useTool(tool)`** accepts a `defineTool` value or an inline object and runs the same checks. It pushes the tool onto the current render frame: `frame.tools.push(tool)` ([`FLUE/packages/runtime/src/hooks/use-tool.ts#L65-L69`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/hooks/use-tool.ts#L65-L69)). Tools declared inside custom hooks land in the same flat set.
- **Duplicate names fail fast, twice.** First, `assertUniqueToolNames(frame)` throws `ToolNameConflictError { conflict: 'duplicate' }` at the end of each render ([`hooks/render.ts#L88`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/hooks/render.ts#L88), [`#L227-L236`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/hooks/render.ts#L227-L236)). Second, `assembleModelTools` checks again across the built-in, framework, custom and result groups, and rejects reserved names (`task`, `activate_skill`, `read_skill_resource`, `finish`, `give_up`) ([`session.ts#L4188-L4231`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/session.ts#L4188-L4231)).
- **Input validation** runs in `parseToolInput`, which parses with Valibot before `run` and throws `ToolInputValidationError` ([`tool.ts#L159-L183`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/tool.ts#L159-L183)). The JSON schema the model sees is `valibotToJsonSchema(input)`. Without an `input` it is `{type: 'object', properties: {}, additionalProperties: false}` ([`session.ts#L4093-L4096`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/session.ts#L4093-L4096)). `output`, when given, is validated too, and the result must be the envelope `{ output?, terminate? }` ([`tool.ts#L231-L330`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/tool.ts#L231-L330)).
- **Errors** do not end the run. "A throw inside `run` does not crash the agent. It becomes an error result the model sees". A validation failure means "`run` is never called — the failure goes back to the model as a tool error" ([`FLUE/apps/docs/src/content/docs/guide/tools.md`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/apps/docs/src/content/docs/guide/tools.md), "How a tool call works"). When `timeoutMs` expires, the call settles with a `ToolTimeoutError`, and "the conversation continues".
- **Per-turn tool set.** "The agent function re-renders before every model call, and each render declares its tool set from scratch" (tools guide, "Conditional tools"). `prepareRerenderTurn` re-renders the agent function, sets `this.agentTools = next.tools`, rebuilds the list with `assembleModelTools`, and assigns it to `this.agentLoop.state.tools` ([`session.ts#L939-L973`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/session.ts#L939-L973)). Changes to the set are narrated to the model as a `resources` signal ("New tool available: …"). The guide warns that changing the set invalidates the provider's prompt cache.
- **Closures over render-time values.** Because each render rebuilds every inline `useTool({... run })`, `run` captures that render's values. In the guide, `record_approval` calls the `setApproved` from the current render, and `useTool(publishRelease)` exists only while `approved` is true (tools guide, "Conditional tools"). The render code says per-turn re-renders "refresh the closures the same way tools and instructions refresh" ([`hooks/render.ts#L88-L92`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/hooks/render.ts#L88-L92)).
- **Parallel execution.** The session creates its loop with `toolExecution: 'parallel'` ([`session.ts#L2285`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/packages/runtime/src/session.ts#L2285)). The loop itself lives in the external `@earendil-works/pi-agent-core` package, which was not read here. Flue's behaviour reference says "tool calls in one batch run in parallel", and that the built-in `write`/`edit` tools take a per-path lock ([`reference/agent-behavior.md#L51-L55`](https://github.com/withastro/flue/blob/deee93b40d275ecc9a50c50af2158f20089d54bc/apps/docs/src/content/docs/reference/agent-behavior.md#L51-L55)). `terminate: true` ends the turn "once the current tool batch settles".

## 3. Side by side

| Concern | Flue | Rig 0.45 |
| --- | --- | --- |
| Definition | `defineTool` object, or an inline `useTool({...})` | `impl Tool` (const `NAME`), `DynamicTool`, or `#[rig_tool]` fn |
| Input schema | Valibot, converted to JSON Schema | Hand-written `Value`, or schemars via `args_schema::<A>()` |
| Argument validation | Valibot parse before `run`; failure is a tool error | serde deserialise only; `validate()` exists but the loop doesn't call it |
| Output | `{ output?, terminate? }`, optional `output` schema | `IntoToolOutput` (text, JSON, multimodal) |
| Duplicate names | Throw (render and assembly); reserved names too | Last one wins, with a `warn!` |
| Set per turn | Re-render rebuilds it; changes narrated | Fresh snapshot per model call; hook can mutate it or set `active_tools` |
| Parallel | Default `'parallel'` | Default 1; `tool_concurrency(n)`; history kept in call order |
| Errors | Throw, validation failure or timeout → error result; run continues | `ToolExecutionError` (kind, feedback, retryable) → `is_error` result; run continues |
| Per-call context | `{ data, signal, log, toolCallId, harness?, step? }` | `&mut ToolContext` (serialisable inbound values plus result metadata) |

## 4. Options for a keelrs `cx.use_tool(..)` (not decisions)

keelrs's agent function is `Fn(&mut AgentContext) -> String + Send + Sync`. It runs again before every turn and must be synchronous ([`keelrs/src/agent.rs`](../../keelrs/src/agent.rs)). `use_tool` would push a tool declaration onto `AgentContext`, the same way `use_skill` pushes a `Skill`.

**What to accept from the author.**

- **Option A: a keelrs-owned `Tool` trait.** It would mirror Rig's (`NAME`, `Args: DeserializeOwned + JsonSchema`, `Output`, `call`). keelrs would adapt it to `rig::tool::Tool`, or would build a `DynamicTool` from it with `args_schema::<Args>()`. This keeps Rig out of the public API and gives typed tests.
- **Option B: a builder that closes over render-time values**, for example `cx.use_tool(tool("lookup_order", "…", move |args: LookupArgs| { let db = db.clone(); async move { … } }))`. Inside, this is a `DynamicTool` whose callback deserialises `Args` and serialises the output. It is the closest match to Flue's inline `useTool({... run })`. Each capture must be `'static + Send + Sync`, which in practice means cloning `Arc`s.
- **Option C: accept Rig types directly** (`impl rig::tool::Tool` / `DynamicTool`). This is the least code, but it ties the public API to Rig, which has a history of breaking changes (see `MIGRATING.md`).

**How to get the tools to Rig each turn.**

1. **keelrs drives the loop.** For each turn, it runs the agent function, builds a fresh `ToolSet` from the declarations, sends `CompletionRequest.tools(set.tool_definitions())`, dispatches the calls with `ToolSet::execute` (concurrently, if keelrs wants Flue's default), and records each result. This gives keelrs full control over when tool calls and results are durably recorded (the sqlx direction). Closures are naturally fresh every turn, because the set is rebuilt from that turn's render.
2. **Rig's `AgentRunner` with a keelrs hook.** In `on_completion_call`, the hook re-runs the agent function, reconciles a shared `ToolServerHandle` (`add_dynamic_tool` / `remove_tool`), and returns `RequestPatch::new().preamble(..).active_tools(names)`. Rig then pins the snapshot for the turn and handles concurrency, history order and resumable partial batches. The cost is that keelrs depends on more of `rig-agent`, and the hook must hand data back through the registry rather than return it.
3. **Register everything once and narrow per turn with `active_tools`.** This only works for tools that don't close over per-render values, so on its own it does not cover Flue's conditional and inline tools.

**Behaviour keelrs would have to add itself, whichever option it picks:**

- A duplicate-name check at declaration or assembly time, since Rig replaces silently. This would include any names keelrs reserves, such as an `activate_skill` tool.
- Schema validation before `run`, if keelrs wants Flue's behaviour (an `InvalidArgs` error result before the body runs). Rig's `tool::validate::validate` or serde can do this.
- A choice of default concurrency. Flue runs tools in parallel; Rig defaults to sequential.
- Narrating changes to the tool set to the model (Flue's `resources` signal). Rig does nothing equivalent.

## Open questions

- Does keelrs want `rig-agent` (and its hook and registry model) as a dependency, or only `rig-core` (providers, `CompletionRequest`, `Tool`, `DynamicTool`, `args_schema`)? `ToolSet` lives in `rig-agent`. With only `rig-core`, keelrs would keep its own name-to-tool map.
- How does the timeout/abort model (`timeoutMs`, `signal`) map? Rig has a `Timeout` / `Cancelled` error kind, but no per-tool deadline field was seen on `Tool`.
- Durable tools (`step.do`) have no counterpart in Rig's `Tool`. Rig 0.45's CHANGELOG mentions "resumable in-flight model calls and partial tool batches" ([`CHANGELOG.md#L28`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/CHANGELOG.md#L28), [`#L46`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/CHANGELOG.md#L46)). That was not investigated here.
