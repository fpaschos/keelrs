# How Rig's output policy delivers a typed result on `AgentRun`, and what keelrs would need

Research for issue #30 (map: #29). It reads, from source, how Rig `0.46.0-rc.1`'s output policy turns a JSON schema into a final value on the sans-IO `AgentRun` path keelrs drives: how the mode is chosen, how the value reaches `AgentRunStep::Done`, what happens when the model skips the output tool or sends a bad value, how the reserved name meets keelrs's tool names, and what Anthropic's wire declares. It then compares Flue's `result:` option and fluex's `useStructuredResult`, and the `Report<T>` workaround in `examples/dogfood`. Sections 1 to 9 are facts with citations. **Section 10 is inference**: what the "Structured result API" grilling (#34) has to choose. It makes no decisions.

It builds on earlier notes and does not repeat them:

- [`rig-agent-loop.md`](https://github.com/fpaschos/keelrs/blob/research/rig-agent-loop/docs/research/rig-agent-loop.md) (branch `research/rig-agent-loop`): `AgentRun` as a serializable sans-IO state machine emitting `CallModel` / `CallTools` / `Done`, and that it owns the "output policy". This note opens that policy up.
- [`agent-loops.md`](https://github.com/fpaschos/keelrs/blob/research/agent-loops/docs/research/agent-loops.md) (branch `research/agent-loops`) and [`tool-call-ordering.md`](https://github.com/fpaschos/keelrs/blob/research/tool-call-ordering/docs/research/tool-call-ordering.md) (branch `research/tool-call-ordering`): how the loop and a reply's calls run. Not repeated.

House rule: this note names only keelrs, Rig, Flue and fluex. The agent-loop library Flue builds on is described generically.

**Sources.**

- Rig: tag [`v0.46.0-rc.1`](https://github.com/0xPlaygrounds/rig/tree/v0.46.0-rc.1) (commit `6eff50a2f`), extracted with `git archive`. The published crates keelrs compiles against (`~/.cargo/registry/src/*/rig-{core,agent}-0.46.0-rc.1`) are byte-identical to the tag for every file cited here. `CORE/` and `AGENT/` stand for `https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/{rig-core,rig-agent}/`. The local checkout is 21 commits past the tag; the changes that touch this topic are listed in §8.
- Flue: local checkout at tag `v2.3.0` (commit `deee93b4`). `FLUE/` stands for `packages/runtime/src/`.
- fluex: working tree at `45b6874` (2026-10-26). `FLUEX/` stands for `packages/flue-tools/src/`.
- keelrs: `main` at `2404e09`. `KEELRS/` stands for `keelrs/src/`.

## Summary

- **Four modes, chosen per turn by `prepare_request`.** `RunSpec.output_schema` turns the policy on; `output_mode` is `Auto` (default), `Tool`, `Native` or `Prompted`. `Auto` becomes `Tool` only when the turn has executable tools, the tool choice allows the output tool, **and** the provider says native output does not compose with tools; otherwise `Native`.
- **On keelrs's Anthropic path `Auto` resolves to `Native`.** Rig's Anthropic Messages wire declares `composes_native_output_with_tools = true` for every model (a wire-level constant, not a catalog fact), so `claude-sonnet-5-5` gets the schema as Anthropic's `output_config.format` on every turn, and no output tool. `Tool` mode must be asked for explicitly.
- **`Done` carries the value as text.** `PromptResponse::output()` is the final turn's text; in Tool mode Rig replaces the turn's prose and calls with one text part holding the output tool's arguments as JSON, and `output_tool_calls()` counts the calls. Nothing is deserialized at the `AgentRun` level; typing into `T` is the driver's job.
- **Validation is shallow, and only in Tool mode.** Rig checks that the output tool's arguments are a JSON object and contain the schema's **top-level `required`** fields. Types, nested fields and other constraints are never checked. Native mode checks nothing at all (the provider's constrained decoding is the check).
- **Skips are re-asked, then accepted.** In Tool mode, a text-only final turn that is not already schema-shaped JSON gets one reprompt (`DEFAULT_OUTPUT_RETRIES = 1`, only via `from_spec`); when the budget runs out the run **finishes successfully with plain text** and `output_tool_calls() == 0`. Missing required fields likewise finish best-effort. Only non-object arguments fail the run. There is no give-up path.
- **The reserved name is `final_result`, renumbered on collision.** Rig picks `final_result`, `final_result_1`, … against the turn's tools and pins the first name it uses; a later turn whose tools take the pinned name fails preparation. The output tool is "allowed but never executable", so it never appears in `CallTools`; sibling calls in the same reply are dropped.
- **keelrs today sets none of it.** It builds `AgentRun::new` (output retries 0, no schema), a `RunSpec::new()` without schema, passes `None` as `committed_output_tool`, and reads `response.output()` as the reply text.
- **Flue** injects `finish` + `give_up` per call, validates the full schema in the tool (errors go back to the model), ends the loop with `terminate`, re-asks without a cap below 32, and throws `ResultUnavailableError` on give-up or exhaustion. **fluex** mounts one `structured_result` tool per agent, writes the value to a data part, and enforces the call with `useAgentFinish`; the caller reads it from the reply.

## 1. The settings and where they go

`RunSpec` (`AGENT/src/run/spec.rs#L52-L64`):

```rust
pub output_schema: Option<rig_core::schemars::Schema>, // "JSON schema the final answer must satisfy"
pub output_mode: OutputMode,                            // "How structured output is obtained."
pub output_tool_name: Option<String>,                   // "None lets the driver pick."
pub output_tool_description: Option<String>,
pub augment_output_preamble: bool,                      // default true (L109)
```

`pub const DEFAULT_OUTPUT_RETRIES: usize = 1` (`spec.rs#L127-L132`): "the default a run built from a spec carries; drivers that construct runs by hand pass it explicitly".

`OutputMode` (`AGENT/src/run/output.rs#L11-L30`), `#[default] Auto`:

| Mode | Doc |
|---|---|
| `Auto` | "Resolve to tool output when a schema and function tools are present, tool choice permits it, and native output cannot compose with tools. Otherwise use native output." |
| `Tool` | "Offer the schema as a synthetic final-answer tool … Calling it is best-effort; callers must validate the result." |
| `Native` | "Send a native structured-output constraint on every turn. Enforcement and compatibility with tool calls depend on provider capabilities." |
| `Prompted` | "Put the schema in the system prompt and return final text verbatim. Callers must extract and validate JSON." |

The spec reaches the two halves of the protocol separately:

- **Request side.** `prepare_request(spec, capabilities, history, tools, committed_output_tool: Option<&str>, patch)` (`AGENT/src/run/prepare.rs#L121-L128`) reads `output_schema`, `output_mode`, `output_tool_description` and `augment_output_preamble` (L137-L140). It does **not** read `spec.output_tool_name`; the name comes only from `committed_output_tool` or is generated (L189-L197).
- **Run side.** `AgentRun::from_spec` (`AGENT/src/run/mod.rs#L1725-L1748`) copies the schema into `with_output_validation(schema, DEFAULT_OUTPUT_RETRIES)` and, when `spec.output_tool_name` is set, calls `with_output_tool_name(name)`. `AgentRun::new` (L393-L412) starts with `output_schema: None`, `max_output_retries: 0`, `output_tool_name: None`.
- `with_output_tool_name` "only seeds `output_tool_name`, which the driver passes to `prepare_request` as `committed_output_tool` for later turns. It does not by itself make a call to the tool finalize the run: a turn intercepts the call only when its `TurnPolicy` names the tool" (L565-L578). `AgentRun::output_tool_name()` returns the pinned name (L580-L586); `model_response` pins the first name a turn's policy carries and never unpins it (`pin_output_tool`, L588-L595, called at L1062).
- Rig's own driver does exactly this: "Pin output mode across registry changes between turns. `let committed_output_tool = run.output_tool_name()`" (`AGENT/src/agent/engine.rs#L329-L339`).

## 2. Mode resolution and the request (`prepare_request`)

`resolve_output_mode` (`prepare.rs#L321-L343`):

- no schema → `Native` (i.e. nothing is sent);
- `Native` → `Native`; `Prompted` → `Prompted`;
- `Tool` → `Tool` if the output tool is callable under the tool choice, else **falls back to `Native`**;
- `Auto` → `Tool` iff `has_executable_tools && output_tool_callable && !provider_composes_native`, else `Native`.
- A committed output-tool name with a schema forces `Tool` regardless (L198-L199).

"Callable" (`CORE/src/structured_output.rs#L68-L78`): no choice, `Auto`, `Required`, or a `Specific` set naming it; `ToolChoice::None` forbids it. When a pinned Tool run meets a choice that forbids the tool, `prepare_request` only logs a warning (L225-L236).

What the resolved mode puts on the request:

- **Tool.** The synthetic definition is appended **last** to `tools`, with `parameters` = the schema and `description` = `output_tool_description` or `OUTPUT_TOOL_DESCRIPTION` (L270-L281). Text: "Call this tool exactly once with your final answer when you are done. Its arguments are the structured result and must satisfy the output schema." (`structured_output.rs#L17-L18`). If `augment_output_preamble`, the preamble gets `output_tool_augmentation(name)`: "When you have gathered enough information to answer, call the `{name}` tool exactly once with your final answer … Do not return the final answer as plain text." (`prepare.rs#L238-L258`, `structured_output.rs#L23-L29`). `output_schema` on the request stays `None`.
- **Native.** `PreparedRequest.output_schema = Some(schema)` (L283-L286); `apply` writes it onto the `CompletionRequest` (L97-L113). No tool, no preamble change.
- **Prompted.** The canonical schema JSON is appended to the preamble with "Respond with ONLY a single JSON object …" (L245-L249, `structured_output.rs#L31-L37`).
- `PreparedRequest.output_mode` records the mode the turn actually runs in, never `Auto` (L80-L81). `PreparedRequest.policy` (a `TurnPolicy`) carries the output tool's name (L82-L84); the driver must feed exactly that policy back via `ModelTurn::from_policy(&response, policy)`, as keelrs already does.

`TurnPolicy::resolve` inserts the output tool into `allowed` but not `executable`: "The output tool is allowed, so its call is not invalid, though it never executes" (`AGENT/src/run/policy.rs#L194-L198`, accessor L224-L228).

## 3. What `AgentRun` does with the reply (`next_step`)

All in `AGENT/src/run/mod.rs`, state `AwaitingAdvance`:

**The model called the output tool** (L896-L972), only when the turn's `policy.output_tool()` is `Some`:

- The **first** call to that name is the answer; `output_tool_calls` counts every call to it in the turn (L907-L917). "The first output-tool call is the answer, not executable work; sibling calls must not run after finalization" (L896-L897): the other calls of the reply never reach `CallTools`.
- Arguments that are not a JSON object: reprompt with `invalid_arguments_feedback` (as a tool result on that call id) while the budget lasts, else **fail** with `ProviderError::Response("the output tool `…` was called with arguments that are not a JSON object: …")` (L921-L939).
- Missing top-level required fields: reprompt with "The `{name}` arguments were missing required field(s): … Call `{name}` again with every required field." (L940-L956, `structured_output.rs#L47-L54`). The feedback is the tool result of the output call; sibling calls get "Tool not executed because another tool call in the same assistant turn was invalid." (`CORE/src/tool/feedback.rs#L83-L106`). Past the budget, the run **finalizes with the incomplete arguments**.
- On acceptance the history stores the turn **as text without tool calls** ("so resumed history cannot contain unanswered calls", L958-L966): the assistant message's prose plus one text part with the JSON. The response's `content` drops prose and calls and keeps reasoning/images plus the JSON text (`AGENT/src/run/response.rs#L199-L225`).

**The model answered with text only** (L1002-L1019): when the turn has an output tool, the text is not blank, the budget remains, and the text does **not** already parse as JSON with every top-level required field (`text_satisfies_schema`, `structured_output.rs#L98-L107`), the run commits a user message "Provide your final answer by calling the `{name}` tool with the structured result as its arguments, not as plain text." (`structured_output.rs#L39-L45`) and re-asks. Otherwise it **finishes with the text** (`self.finish(items, 0)`). A blank turn finishes without a reprompt.

**Budget.** `can_reprompt_for_output` = `output_retries < max_output_retries && current_turn < max_turns` (L525-L530); each reprompt costs a model turn. The count resets whenever a tool batch runs: "Output retries are budgeted per finalization attempt, not per run" (L995-L996). `with_output_validation`'s doc: "Validation checks only top-level required field presence; exhausting either the output or model-call budget finalizes best-effort, except that arguments that are not a JSON object fail the run" (L503-L515).

**Native and Prompted** turns have no output tool in their policy, so neither branch above runs: the final text is accepted as is (L1005 requires `policy.output_tool()`).

**Errors.** `PromptError` has no structured-output variant (`response.rs#L235-L300`: `Provider`, `Report`, `Memory`, `MaxTurns`, `Cancelled`, `UnknownToolCall`). The only failure the output policy raises is the `ProviderError::Response` above. There is no give-up tool, outcome or error anywhere in the policy.

## 4. How `Done` exposes the value

`AgentRunStep::Done(PromptResponse)` (`mod.rs#L96-L98`). On `PromptResponse` (`response.rs#L80-L167`):

- `content: Vec<AssistantContent>`: "In output-tool mode the model's prose and tool calls are replaced by one text part holding the serialized structured output" (L98-L103).
- `output() -> String`: "The concatenated text parts of `content`. In output-tool mode this is the serialized structured output" (L163-L167).
- `output_tool_calls() -> usize`: how many times the output tool was called in the finalizing turn (L156-L161). The field is `#[serde(skip)]` (L104-L108), so it does not survive serialization. `0` means the run finished on text (a skip, a Native/Prompted run, or schema-shaped text accepted without a call).
- `messages`: the run's transcript, with the output turn stored as text (§3).

So a driver can tell "the model called the tool" (`> 0`) from "the model ended in text" (`== 0`), but in Tool mode a `0` with valid JSON text and a `0` after an exhausted budget look the same except for parsing.

## 5. Rig's own typed layer (what `AgentRun` leaves to the driver)

Rig types the value one layer up, in `AgentRunner`, which keelrs does not use (ADR 0004):

- `Agent::prompt_typed::<T>()` → `TypedRun::native`: sets `output_schema = schema_for!(T)` and **forces `OutputMode::Native`** ("Native typed prompting parses final text rather than requiring an output call") (`AGENT/src/agent/completion.rs#L689-L694`, `AGENT/src/agent/typed.rs#L114-L122`).
- `Extractor::extract` → `TypedRun::output_tool`: output tool `submit`, `ToolChoice::Required`, `OutputMode::Tool`, `max_turns(1)`, unhandled invalid calls ignored (`AGENT/src/extractor.rs#L77-L85`, `#L127-L137`; `typed.rs#L124-L131`). For a model that `rejects_forced_tool_choice`, the runner drops the forced choice and, where native composes with tools, asks for the JSON as text instead (`AGENT/src/agent/completion.rs#L140-L176`).
- `recover_output` (`typed.rs#L204-L241`): Native → empty is `EmptyResponse`, else parse the trimmed text, falling back to the first `{`/`[` (`deserialize_structured_output`, L253-L271). Tool → if `output_tool_calls() == 0`, accept text that parses as `T`, else `EmptyResponse` ("The submit tool was not called"); if `> 1`, warn and use the first.
- `TypedRun::retries(n)`: a failed attempt (run error, empty, unparseable) is **re-run from scratch** (L142-L150, L152-L200).
- `StructuredOutputError { Prompt(PromptError), Deserialization { output, error }, EmptyResponse }` (`AGENT/src/completion.rs#L21-L38`).
- `AgentRunner::output_tool(name, description, augment)` is `pub(crate)` (`AGENT/src/agent/runner.rs#L103-L115`); the public builder exposes `output_schema::<T>()`, `output_schema_raw` and `output_mode` (`AGENT/src/agent/builder.rs#L326-L345`).

## 6. Anthropic and `claude-sonnet-5-5`

- `ProviderCapabilities { composes_native_output_with_tools, rejects_forced_tool_choice }`, both default `false` (`CORE/src/completion/request.rs#L1163-L1196`). keelrs reads them as `model.capabilities().completion` (`KEELRS/agent/rig/mod.rs#L135-L142`); `DynModel::capabilities` is the wire's `describe().capabilities` (`CORE/src/driver/dyn_model.rs#L145-L148`).
- Anthropic's `Messages` wire (the provider's completion wire, `CORE/src/providers/anthropic/wire.rs#L334-L336`) declares `.with_native_output_tool_composition(true)` unconditionally, commented "Constrained output decoding does not suppress strict tool calls", and `rejects_forced_tool_choice` from the catalog (`wire.rs#L598-L612`). Other wires that declare `true`: Cohere chat, Ollama chat, and OpenAI chat / Responses conditionally (`grep with_native_output_tool_composition`).
- The catalog marks `claude-sonnet-5-5` with `"structured_output": true` and `"rejects_forced_tool_choice": true` (`CORE/src/catalog/models.json#L25`; same for `claude-opus-5-5`, L18). `structured_output` plays no part in `resolve_output_mode`; only the wire constant does.
- So with `output_mode: Auto` and any tools, every keelrs turn on `anthropic/claude-sonnet-5-5` resolves to **Native**. Anthropic's encoder then sends `"output_config": {"format": {"type": "json_schema", "schema": …}}` on every turn (`CORE/src/providers/anthropic/completion.rs#L313-L317`, L338), after `sanitize_schema`: "Require all object properties, disallow additional properties, and remove numeric constraints" (L987-L997).
- `OutputMode::Tool` with `ToolChoice` unset (`None`) stays callable and resolves to Tool on Anthropic too. Forcing the tool (`Required`/`Specific`) is what `rejects_forced_tool_choice` warns against; only Rig's extractor flow reacts to that flag (§5).
- Not established from source: how Anthropic's constrained decoding treats the text of an intermediate turn that also calls tools, or a final turn after many tool rounds. Rig's cassette tests for Anthropic structured output cover the Tool and Prompted modes mapping onto the wire (`crates/rig-cassette/tests/providers/anthropic/cassette/structured_output.rs`), not a live multi-turn Native run with tools.

## 7. The reserved name and keelrs's tool names

- Rig: `output_tool_name(is_taken)` returns `final_result`, then `final_result_1`, `final_result_2`, … while the name is taken (`structured_output.rs#L56-L66`). Taken = the turn's executable tool names, or the pre-`active_tools` set when an allow-list filters (`prepare.rs#L162-L197`). Once pinned, a real tool with the same name in a later turn is a `PrepareError::Request("real tool `…` conflicts with the structured-output tool reserved for this run; …")` (L213-L223). Rig does not reserve the name in any registry; it only avoids and checks.
- A call to the output tool in a turn whose policy does not name it (e.g. a Native turn) is an ordinary unknown tool call (`mod.rs#L568-L574`).
- keelrs: `AgentContext::tools()` rejects a declared tool named like a skill tool (`activate_skill`, `read_skill_resource`) or any built-in, and duplicate names; the run fails with that reason (`KEELRS/agent/context.rs#L9`, `#L127-L140`). Nothing reserves `final_result` today. The agent function runs before every turn, so the tool set can change between turns (ADR 0004, `docs/adr/0004-keelrs-drives-rigs-agentrun.md`).
- keelrs's driver today (`KEELRS/agent/rig/mod.rs`): `AgentRun::new(..).with_history(..).max_turns(..)` (L70-L72), so output schema `None` and output retries `0`; `RunSpec::new()` with preamble and reasoning only (L128-L134); `prepare_request(.., None, None)` (L135-L143), i.e. no committed output tool; unknown calls are skipped with "tool \"…\" is not available; available tools: …" (L152-L166); `Done` → `Settled { reply: response.output(), .. }` (L180-L188). `Reply` exposes only `text()` and `submission_id()` (`KEELRS/agent/instance.rs#L230-L248`); `ask` = dispatch + read (L74-L77). One `AgentRun` is one dispatch, so Rig's output policy, its pin and its retry budget are **per dispatch**.

## 8. Changes in the 21 commits after the tag

- `8d6205e51` (provider-hosted tools): `RunSpec.provider_tools`; `resolve_output_mode`'s `has_executable_tools` becomes `has_tools` = executable **or** hosted, so hosted tools alone can make `Auto` pick Tool mode; `TurnPolicy` gains `hosted` so `Required` is satisfiable by hosted tools (`git diff v0.46.0-rc.1 HEAD -- crates/rig-agent/src/run/{prepare,policy,spec}.rs`).
- `55cd481a2` / `3cb43192a`: the Anthropic `Messages` wire's composition flag becomes `dialect.quirks.supports_output_format` instead of `true`, and the encoder drops `output_schema` with a warning when the dialect lacks it. Anthropic's own dialect keeps `true`; at least one other Messages-protocol dialect sets `false` (`git show HEAD:crates/rig-core/src/providers/anthropic/wire.rs`, L76, L126, L143, L287, L716). For `anthropic/claude-sonnet-5-5` the outcome is unchanged: Native.
- No change to `run/mod.rs`'s output path, `structured_output.rs`, `typed.rs` or `extractor.rs`.

## 9. Flue, fluex and keelrs's dogfood

**Flue `result:`** (per call). `session.prompt(text, { result: schema })` resolves `PromptResultResponse<T> { data, usage, model }` (`FLUE/types.ts#L740-L750`, `#L914-L926`; same option on `skill()` and `task()`).

- Per call, `createResultTools(schema)` makes two tools, `finish` (parameters = the schema, non-object schemas wrapped as `{ result }`) and `give_up` (`{ reason: string, minLength 1 }`) (`FLUE/result.ts#L13-L14`, `#L170-L306`, `#L319-L323`), passed as the call's `extraTools` (`FLUE/session.ts#L5874-L5882`).
- A footer is appended to the prompt/skill text: call `finish`, or `give_up` with a reason; "Do not respond with the answer in plain text" (`result.ts#L17-L24`, used at L70, L108-L110, L115-L123).
- `finish` validates the **full** schema (valibot `safeParse`, with transforms/defaults) and throws "Result does not match the required schema: … Please call `finish` again with a corrected payload." The throw becomes an error tool result the model reads (L202-L224). First success wins; later calls get "A result was already submitted …" (L203-L209, L332-L351). A successful `finish`/`give_up` returns `terminate: true`, ending the loop after the batch (L225-L237, L266-L276).
- `runWithResultTools`: after each loop, read the outcome; `finished` → the value; `gave_up` → `throw new ResultUnavailableError(reason, assistantText)`; `pending` → send "You ended your turn without calling `finish` or `give_up`. Either call …" and loop, at most `MAX_FOLLOWUPS = 32`, then throw `ResultUnavailableError("Agent did not call … after 33 attempts.")` (`session.ts#L5960-L6010`, `result.ts#L26-L32`, `#L360-L368`). "There is no retry cap from the framework's perspective: the model has a clear escape hatch via `give_up`" (L5966-L5969).
- `finish` and `give_up` are framework-reserved: a custom tool with either name throws `ToolNameConflictError` (`session.ts#L4193-L4222`).

**fluex `useStructuredResult`** (per agent, `FLUEX/structured-result.ts`, 58 lines).

- `useStructuredResult(name, schema, description = 'Submit your result. Call once, as the very last step.')`, called on every render: mounts one tool named `structured_result` whose input is the schema (Flue validates it and returns mismatches to the model), writes the value to the data part `name`, and returns `{ output: 'Submitted.', terminate: true }` (L10-L35). "A second call in one render throws (duplicate tool name), which keeps it to one result per agent" (L13-L15).
- Enforcement through `useAgentFinish`: if no non-error `structured_result` call happened in the response, append a signal "You have not called `structured_result`. Call it now with your result." (L37-L45). Flue's ceiling is `MAX_AGENT_FINISH_CYCLES = 32`, after which the response fails (`FLUE/session.ts#L250-L256`, `#L1671-L1675`). No give-up tool.
- `readStructuredResult(name, reply, schema)` takes the last value under `name` from `reply.data`, parses it again, and throws if absent (L48-L58).

**keelrs `examples/dogfood`'s `Report<T>`** (on `main`, `examples/dogfood/src/report.rs`, 63 lines, plus three `LazyLock<Report<_>>` statics and three `use_tool` lines in `main.rs#L66-L93`):

- A host tool (`report_plan`, `report_implementation`, `report_review`) that writes `call.data` into an `Arc<Mutex<Option<T>>>`, answering "Recorded."; a second call replaces the first (L36-L47). The schema is `T`'s, so keelrs's `arguments_refusal` checks it before the tool runs (`KEELRS/agent/rig/mod.rs#L211-L214`).
- `ask` clears the slot, asks, takes the value; if absent, asks once more with "You ended without calling `{name}`. Call it now." and errors if still absent (L49-L62). No give-up; the tool does not end the run (the model may keep going after it).

| | Rig `AgentRun` Tool mode | Rig Native (keelrs on Anthropic) | Flue `result:` | fluex `useStructuredResult` | dogfood `Report<T>` |
|---|---|---|---|---|---|
| Scope | per run (= per dispatch) | per run | per call | per agent | per agent (static) |
| Tool name | `final_result[_n]`, avoided | none | `finish`, `give_up`, reserved | `structured_result` | `report_*` |
| Validation | object + top-level `required` | provider constrained decoding | full schema, errors to model | full schema (tool input) | full schema (`arguments_refusal`) |
| Ends the loop | yes, siblings dropped | final text is the value | `terminate` | `terminate` | no |
| Skip | 1 reprompt, then plain text succeeds | n/a | re-ask ≤ 32, then error | re-ask ≤ 32, then failure | 1 re-ask, then error |
| Give up | none | none | `give_up` → `ResultUnavailableError` | none | none |
| Caller reads | `output()` JSON text | `output()` JSON text | `response.data: T` | `readStructuredResult` | `Report::ask -> T` |

## 10. Inference (not decided here)

What the facts mean for "Structured result API" (#34):

- **Native is what keelrs gets by default on Anthropic, and it is the weakest fit for an agent with tools.** It has no tool call to mark "done", no skip to detect, no give-up, and Rig validates nothing; a schema-constrained final text is the only signal. Whether Anthropic's constraint behaves well across many tool rounds is unverified (§6). Getting Flue-like behaviour means forcing `OutputMode::Tool` (no forced `tool_choice`, since the model rejects it) or bypassing Rig's policy with a keelrs-owned tool.
- **Rig's Tool mode covers part of what dogfood does, but not the strict half.** It gives a reserved, collision-free name, ends the run at the call, drops siblings, stores clean history, and re-asks once. It does not fail on a skip (it finishes with text) and validates only top-level `required`. A keelrs API that promises `T` would still need, in the driver: `output_tool_calls() == 0` → re-ask or fail; deserialize `output()` into `T`, and on failure decide between returning the serde error to the model (Rig's run is already `Done`, so this needs a new `AgentRun`, ADR 0004 rule 3) or failing the dispatch.
- **To use Rig's Tool mode, keelrs's driver would set:** `spec.output_schema`, `spec.output_mode = Tool` (and maybe `output_tool_description`); construct the run with `with_output_validation(schema, n)` (it uses `AgentRun::new`, not `from_spec`); optionally `with_output_tool_name(name)`; pass `run.output_tool_name()` as `committed_output_tool` each turn; and on `Done` read `output()` plus `output_tool_calls()`. All of it stays inside `agent::rig` (ADR 0004 rule 2).
- **A keelrs-owned tool (the dogfood/Flue/fluex shape) is the alternative.** keelrs already validates arguments against the full schema before a tool runs and sends the refusal to the model, which Rig's output tool does not. What keelrs lacks for that shape is a way for a tool to end the run (`terminate`) and a place on the `Reply` (or a typed read) to carry the value.
- **Per call vs per agent.** Rig's policy is per `AgentRun` = per dispatch, which matches Flue's per-call `result:` and fits a typed `ask`/`read`. fluex's per-agent shape matches keelrs's agent function re-declaring tools each turn, but then every dispatch to that agent must end in a result. dogfood uses one `Report` per agent but asks per dispatch.
- **Reserved names.** If Rig's tool is used, `final_result` is avoided per turn but not reserved across turns: a keelrs tool named `final_result` that appears after the pin fails the dispatch at preparation. keelrs could add the name (and a `give_up`, if adopted) to `context.rs`'s reserved list, as it does for built-ins, or keep Rig's renumbering.
- **Skip handling and give-up.** Options the facts lay out: Rig's one reprompt then text; dogfood's one extra ask then error; Flue/fluex's re-ask up to 32 then error, with or without a `give_up` escape. A give-up path exists only in Flue, and would be keelrs's to build (a second reserved tool, an `Outcome::Failed`-style settlement carrying the reason).
- **Boilerplate at stake.** dogfood's `report.rs` (63 lines) and its three statics plus three `use_tool` lines are what a keelrs API would absorb; the public surface it adds is the comparison the AGENTS.md metrics ask for.

Open for #34: per call or per agent; how the caller reads `T` (a typed `ask`/`read`, or a value on `Reply`); Rig's Tool mode vs a keelrs-owned tool vs Native; how many re-asks on a skip before `Failed`; whether to add `give_up`; which names to reserve; whether a schema mismatch goes back to the model or fails the dispatch.
