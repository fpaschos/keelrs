# Research: Rig's providers and models for `use_model`

Ticket: #9 (map #7). Date: 2026-10-26.

Question: how does `cx.use_model("anthropic/claude-sonnet-5-5")` (`examples/hello-world/src/main.rs`) become a Rig model? This note covers Rig's provider clients and model types, building a model from the environment or from config, choosing the model per turn, streaming, and model-call settings. It also covers how Flue parses `provider/model` strings, registers providers and resolves credentials. It gathers facts for the decision ticket and does not make the decision.

Sources:
- **Rig** `0xPlaygrounds/rig` at tag `v0.45.0` (commit `98ed06d`, 2026-10-19), the latest stable release. `rig-core` 0.45.0 is the newest stable version on crates.io (`max_stable_version`, checked 2026-10-26). `v0.46.0-rc.1` is a prerelease and was not read. Paths below are relative to `crates/rig-core/src/` unless they say otherwise. Links point at the tag, e.g. [`providers/registry.rs`](https://github.com/0xPlaygrounds/rig/blob/v0.45.0/crates/rig-core/src/providers/registry.rs).
- **docs.rs**: [`rig-core` 0.45.0](https://docs.rs/rig-core/0.45.0/rig_core/), [`rig-agent` 0.45.0](https://docs.rs/rig-agent/0.45.0/rig_agent/).
- **Flue** `withastro/flue` at `deee93b` (2026-10-12): `packages/runtime/src/runtime/providers.ts`, `runtime/builtin-providers.ts`, `hooks/use-model.ts`, `config.ts`, `session.ts`, `types.ts`. Flue's provider layer is `@earendil-works/pi-ai` `^1.1.0`, locked at 1.1.0 (`pnpm-lock.yaml:79-81`). pi-ai's Anthropic provider was read from the published 1.1.0 npm tarball (`dist/providers/anthropic.js`).

---

## 0. Warning: Rig's provider API was rewritten recently

Docs, blog posts and examples written for Rig ≤ 0.38 describe an API that **no longer exists** in 0.45:

| Older Rig (≤ 0.38) | Rig 0.45 |
|---|---|
| `providers::anthropic::Client` = `client::Client<AnthropicExt>`, built with `Client::builder().api_key(..).build()` | `providers::anthropic::Anthropic`, an `AnthropicConfig` on an HTTP transport (`client/anthropic.rs:11-49`) |
| `ProviderClient::from_env()` / `from_val()` trait | Inherent `Anthropic::from_env()`, `AnthropicConfig::from_env()` (`client/anthropic.rs:27-33`, `providers/anthropic/wire.rs:290-297`) |
| `CompletionClient::completion_model(name)` → `impl CompletionModel` (a trait with associated types, `impl Future` returns, **not** object-safe) | `Anthropic::completion(name)` → `Model<Messages>`; `.erase()` → `DynModel<Completion>` (object-safe, `driver/dyn_model.rs:82-97`) |
| `DynClientBuilder` (dynamic provider by name), deprecated in 0.25.0 (`crates/rig-core/CHANGELOG.md` at `rig-core-v0.38.0`, PR #1105), later removed | `Catalog::connect("vendor/model")` and `catalog::Connector` (added in 0.44.0, 2026-10-07: root `CHANGELOG.md:84`, PR #2750) |
| `rig-core` held the agent | The agent moved to `rig-agent`; `rig` is a facade that re-exports both (README "Crates" table) |

The string-to-model layer (`catalog`, `providers::registry`) is three weeks old and had a breaking change in 0.45.0 already (root `CHANGELOG.md:45`, PR #2769). Expect more churn.

---

## 1. Provider clients and model types

**Facts**

- **`Model<W, T>`** is a concrete model: a `wire: W` (what to send and how to read the reply, e.g. Anthropic's `Messages`) and a `transport: T` (how the payload travels; defaults to `DynHttpClient`) (`driver.rs:53-60`). It has `call(request)` and `stream(request)` (`driver.rs:353, 383`).
- **`DynModel<Op>`** is the type-erased form, `Arc<dyn ErasedModel<Op>>`, made with `Model::erase()` or `From<Model<W,T>>` (`driver/dyn_model.rs:82-110`). Clones share the model. It has:
  - `call(request) -> impl Future<Output = Result<Op::Response, ProviderError>> + Send + 'static`: "the call owns what it sends, so it can be spawned" (`:153-160`)
  - `stream(request) -> Result<Streamed<Op>, ProviderError>` (`:180`)
  - `name()`, `id()`, `spec()` (the catalog facts), `capabilities()`
  - for completions, `check(&request)`, which reports every option the model would refuse without sending anything (`:205-233`)
- **Provider clients** are thin: `Anthropic` is "an `AnthropicConfig` on a transport" (`client/anthropic.rs:11-15`). Its methods are `new(api_key)`, `from_env()`, `completion(model) -> Model<Messages>`, `list_models()` and `verify()` (`:18-49`). `OpenAI` and `Gemini` follow the same pattern (`client/openai.rs`, `client/gemini.rs`). The README quickstart: `OpenAI::from_env()?.completion(openai::GPT_6_1_SOL)` and then `AgentBuilder::new(model)`.
- **Three protocol families** (`registry::Format`): `OpenAi` (Chat Completions and Responses), `Anthropic` (Messages) and `Gemini` (GenerateContent) (`providers/registry.rs:64-104`). Every other vendor is a *dialect* of one of them: deepseek, groq, mistral, openrouter, ollama, xai, zai and others are OpenAI dialects (`registry.rs:39-63`). `ProviderConfig` is a serializable enum over the three families (`registry.rs:719-729`).
- **Agents take erased models.** `rig_agent::AgentBuilder::new(model: impl Into<DynModel<Completion>>)` (`crates/rig-agent/src/agent/builder.rs:520`). Erasure is therefore the normal path, not a workaround.

**Implications for keelrs**
- A keelrs agent can hold one `DynModel<Completion>` per turn regardless of provider. No generics over the provider need to leak into `AgentContext`.
- `DynModel::call` returns a `'static` future, so a turn can be spawned on the runtime without borrowing the agent.

---

## 2. From `"anthropic/claude-sonnet-5-5"` to a model: Rig's catalog and registry

Rig 0.45 already ships a `vendor/model` resolver. It is close to Flue's.

**Facts**

- **Parsing.** A reference is `vendor/model`, **split at the first `/`**, so the model id keeps any later `/` and `:` (`openrouter/anthropic/claude-sonnet-4.5`, `ollama/qwen3:4b`). An older spelling, `vendor/format:model`, is still read (`catalog/mod.rs:171-186`; `Reference::parse`, `:418-437`). An empty vendor or model is rejected.
- **The catalog.** `Catalog::builtin()` is compiled in from models.dev data, with the main providers reviewed by hand. The 0.44.0 changelog counts 1,492 rows across 28 vendors; the data was generated 2026-10-07 (`catalog/generated_at`). Each row holds the model's limits, modalities, reasoning options, cache support, sampling rule and prices (`catalog/mod.rs:1-45`). You can lay a newer models.dev copy over it (`with_models_dev`), or a user's overrides file (`with_overrides`), or insert rows in code (`insert`) (`:195-266`). `anthropic/claude-sonnet-5-5` is in the built-in catalog: output limit 128000, reasoning effort `low…max` plus a toggle, `"temperature": false`, `"sampling": "never"`, `"reasoning_default": "high"` (`catalog/models.json`).
- **Lookup rule.** An id matches as listed, or else as a dated snapshot of the longest listed id it extends with `-20<year>` (`claude-sonnet-4-5-20250929` → `claude-sonnet-4-5`) (`catalog/mod.rs:152-163`). A miss returns `NotFound` with up to five close suggestions (`:175-201`).
- **Connecting.** `Catalog::builtin().connect("anthropic/claude-opus-5-5")` returns `Result<DynModel<Completion>, ConnectError>`. It reads the key from the environment and uses the shared reqwest client (`providers/registry.rs:1027-1047`; docs example at `:11-14`). `connect_with(model, ConnectOptions)` overrides the key (`api_key`), `account_id`, the HTTP client (`http`), the protocol family (`format`) and the `base_url` (`registry.rs:895-961, 1077-1140`).
  - **An unlisted model still connects** if the vendor is known: "A model this catalog does not list still connects, on a vendor it knows" (`registry.rs:1052-1055`). Only the vendor must resolve (`ProviderId::catalog(vendor)`, `:1092-1093`).
  - The model is **encoded with the catalog's facts**. Request options are checked against the row, and replies are priced from it (`Usage::cost`) (`registry.rs:1063-1066`).
- **Credentials from env.** Each dialect names its key variable(s). The first one set to a **non-empty** value wins. A vendor that needs no credential (local Ollama, llama.cpp) gets an empty key. Anything else fails with `ConnectError::MissingKey { vendor, tried }`, and the message names the variables to set (`registry.rs:179-260, 1000-1009`). For Anthropic the variables are `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` (`providers/anthropic/wire.rs:126-127, 290-312`). `ProviderId::api_key_envs()` lists them for a UI (`registry.rs:567`).
- **Secrets are not serialized.** `ProviderConfig` "discards its credential when written". `with_credential(key)` rehydrates a config loaded from data (`registry.rs:1-9, 759-780`).
- **`catalog::Connector`** is the registry an application holds (`catalog/connector.rs:170-186`):
  - `provide(vendor, impl Provide)` registers how every model of a vendor is reached, **before** the environment. `ConnectOptions` itself implements `Provide`, so pointing Ollama at another host is one line. A later `provide` for the same vendor **replaces** the earlier one (`:240-270`).
  - `ProviderId::declare("corp")` adds a vendor at run time, e.g. for a private gateway (`registry.rs:442`; doc example at `connector.rs:243-254`).
  - `add_sign_in(impl SignIn)` is the fallback when no key is found, e.g. a subscription login (`:20-37, 230-234`).
  - `wrap_http(..)` wraps every HTTP client, e.g. for logging or key rotation (`:205-214`).
  - `connect(reference)` returns `(Arc<ModelSpec>, ErasedHandler)` (`:286-325`). `status(spec)`, `reachable()` and `statuses()` tell a model picker which models are usable, which lack a key and which cannot call tools (`:340-407`).
- **Not every vendor connects from rig-core.** Bedrock, Vertex AI, gemini-grpc and candle are "catalog-only": their models live in companion crates, and `connect` returns `ConnectError::CatalogOnly` (`registry.rs:262-305`).

**Implications for keelrs**
- keelrs's string has the same shape as Rig's reference (`vendor/model`, split at the first `/`). `use_model("anthropic/claude-sonnet-5-5")` could be passed to `Catalog::connect` / `Connector::connect` with no keelrs-side parser. Whether keelrs wraps Rig's catalog, uses it directly, or keeps its own registry is the decision ticket's question.
- A missing key surfaces at **connect** time, not at `use_model` time. The agent function returns synchronously (`hello(cx) -> String`), so keelrs has to choose when the connect happens and where its error goes.
- `ProviderConfig` is serde, and serializing it drops the secret. That fits durable agents: store the selection, not the key.

---

## 3. Choosing the model per turn or dynamically

**Facts**

- **By string, at run time.** `Catalog::connect(&str)` and `Connector::connect(&str)` give fully dynamic dispatch by string. There is no compile-time provider type (§2).
- **Same provider, another model, per request.** `CompletionRequest.model: Option<String>` overrides the model id for one request (`completion/request.rs:1201-1203`). The Anthropic encoder sends `request.model` when set, and looks up *that* model's default `max_tokens` (`providers/anthropic/completion.rs:183-195`). This only changes the id on the same wire and credential. It cannot switch providers.
- **Agent-level routing (`rig-agent`).** An agent registers models under `ModelRef` labels: `AgentBuilder::new` (label `default`), `named_model` and `model_route(label, model)` (`crates/rig-agent/src/agent/builder.rs:347-360, 518-530`). Per model call, `AgentHook::on_model_select` returns `ModelSelectionAction::{Continue, Select(label), Stop}`. The hook is **synchronous**, can only pick an **already-registered** label, runs at every model-call boundary (retries and post-tool calls too), and never rebinds an in-flight call (`agent/hook.rs:846-876, 960-977`; `crates/rig-agent/README.md:107-137`). A run can also start on another model with `using_model(label)` / `using_model_value(model)`, and an agent value can swap with `set_model` / `with_model` (`agent/completion.rs:503-515`). The sibling note `docs/research/rig-agent-loop.md` (branch `research/rig-agent-loop`) covers this hook in the loop.

**Implications for keelrs**
- If `use_model` runs on every render (once per turn), keelrs can connect a fresh `DynModel` whenever the string changes, or cache one `DynModel` per reference since they are cheap to clone. Rig does not force a single model per agent.
- If keelrs drives `rig-agent`, a model that is chosen dynamically has to be registered as a label before `on_model_select` can pick it.

---

## 4. Streaming

**Facts**

- Every reply arrives as one stream type, `Streamed<Op>` (`streaming/mod.rs:1-3, 92-103`). `CompletionStream = Streamed<Completion>` implements `futures::Stream` (`:299`). `finish().await` folds it into the full `CompletionResponse` (`:161`), and `partial()` returns what has arrived so far (`:286`).
- `call()` and `stream()` run through the same driver: `DynModel::call` opens in `Mode::Unary` and finishes, `stream` opens in `Mode::Streaming` (`driver/dyn_model.rs:162-197`).
- The items are `Item<StreamEvent>`. `StreamEvent` has `Start`, `Text`, `Reasoning`, `Arguments` (tool-call argument deltas) and `End` variants (`streaming/event.rs:84-118`). Payloads the decoder does not model come through as redacted `UnknownPayload` (`streaming/mod.rs:22-50`).
- `rig-agent` has multi-turn streaming on top (`MultiTurnStreamItem`, `StreamingResult` in `crates/rig-agent/src/prelude.rs:14`).

**Implications for keelrs**
- Whether a turn streams is a call-site choice (`call` vs `stream`), not a model or client type. The same `DynModel` serves both.

---

## 5. Model-call settings

**Facts**

- **On the request** (`completion/request.rs:1201-1246`): `model`, `temperature: Option<f64>`, `max_tokens: Option<u64>`, `tool_choice`, `additional_params: Option<serde_json::Value>` (raw provider JSON) and `output_schema`. Builders include `CompletionRequest::new(prompt).temperature(..).max_tokens(..).reasoning(..).options(..)` (`:1480-1680`). `CompletionRequest` is `Serialize + Deserialize`.
- **Portable options**, `GenerationOptions` (`completion/options.rs:30-80`): `reasoning`, `cache` (prompt-cache retention), `service_tier`, `verbosity`, `parallel_tool_calls`, `top_p`, `seed`, `stop`, `cache_key` and `on_unsupported`. Every wire either maps each option to its own JSON or refuses it. Options are "never silently dropped": the default policy, `OnUnsupported::Error`, fails the request (`:30-35, 94-98`).
- **Thinking / reasoning**: `Reasoning::{Off, Effort(Effort), Budget { tokens }}`, with `Effort::{Minimal, Low, Medium, High, XHigh, Max}` (`completion/options.rs:225-280`). On Anthropic (`providers/anthropic/options.rs:30-90`):
  - `Effort(e)` → `{"thinking":{"type":"adaptive"},"output_config":{"effort":e}}` on models with adaptive thinking (otherwise `output_config.effort` only). The effort must be one of the model's catalog levels. `Minimal` is refused ("Claude has no `minimal` effort level").
  - `Budget { tokens }` → `{"thinking":{"type":"enabled","budget_tokens":n}}`. It is refused when below 1024, when not below `max_tokens`, or when the model only takes effort levels.
  - `Off` → `{"thinking":{"type":"disabled"}}`, or the row's `thinking_off` value. It is refused for models that cannot disable thinking.
- **`max_tokens` on Anthropic** is required by the API. Rig fills it from the catalog's output limit (128000 for `claude-sonnet-5-5`). An unlisted model under the `claude-opus-4`, `claude-sonnet-4` or `claude-haiku-4-5` prefixes gets 64000. Any other unlisted model fails with "`max_tokens` must be set for Anthropic" (`providers/anthropic/completion.rs:90-104, 183-195`).
- **Temperature can be refused.** The catalog's `sampling` rule (`Any`, `ReasoningOff`, `Never`) governs `temperature` and `top_p`. `claude-sonnet-5-5` is `"never"`, so a request carrying `temperature` is refused for it ("the model takes no `temperature`") (`catalog/spec.rs:228-235, 436-452, 530-560`). Per the docs, the catalog check is skipped when the request's `GenerationOptions` are all default (`catalog/spec.rs:448-451`).
- **Pre-flight check**: `DynModel::check(&request)` returns every refusal without sending (`driver/dyn_model.rs:205-233`). `ModelSpec::validate(&options)` checks options against a row (`catalog/mod.rs:36-44`).
- **Agent-level**: `AgentBuilder::{temperature, max_tokens, options}`, the same on `AgentRunner`, and a per-turn `RequestPatch::{temperature, max_tokens, additional_params, ..}` from a hook (`crates/rig-agent/src/agent/builder.rs:225-255`, `agent/runner.rs:260-330`, `run/patch.rs:40-90`).

**Implications for keelrs**
- Flue's `thinkingLevel` vocabulary (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, §6) maps one-to-one onto Rig's `Reasoning::Off` and `Effort::*`. Rig's `Budget { tokens }` has no Flue equivalent.
- Since Rig refuses unsupported options rather than dropping them, a keelrs default such as "always send temperature" would break on current Claude models. The defaults should come from the catalog row or be left unset.

---

## 6. How Flue does it

**Parsing and resolving `provider/model`** (`runtime/providers.ts:193-245`, `resolveModel`)
1. Split at the **first `/`**. A missing slash, an unknown provider id or an empty model id each throws an error that names the fix. The unknown-provider error lists the registered providers and says how to add one.
2. Exact lookup in the provider's declared models (`models.getModel(providerId, modelId)`).
3. Else the **version-separator alias**: a `-` or `.` between digits counts as the same, so `claude-sonnet-4-6` equals `claude-sonnet-4.6`. It only applies when exactly one declared id matches, and it warns once per specifier (`:247-297`).
4. Else, if the provider carries a `DYNAMIC_MODEL_TEMPLATE`, Flue synthesizes a **zero-metadata model**: cost 0, `contextWindow: 0`, `maxTokens: 0`. `isDynamicModel()` tells it apart from a genuinely free model, and Flue warns once per process (`:110-188`).
5. Else it throws "Unknown model ID", listing up to 8 declared ids (`:240-245, 299-304`).

**Registering providers**
- A module-scoped pi-ai `Models` instance (`providers.ts:13-19`). `setProvider(provider)` **replaces** any provider with the same id; calls do not accumulate (`:21-34`).
- Built-ins: the `flue()` config's `providers?: string[]` (provider ids, lowercase and dashes) selects which `@earendil-works/pi-ai/providers/<id>` modules the generated entry imports, so only those ship in the build. If the option is omitted, every built-in is registered (`config.ts:73-80, 113-126`; `builtin-providers.ts:15-19`).
- `registerBuiltinProviderModule(id, ns)` requires exactly one exported `*Provider` factory whose provider id equals `id`. It **skips ids already registered**, so a user's `setProvider()` in `app.ts` wins regardless of module order (`providers.ts:49-85`). `registerDefaultProviders()` skips registered ids the same way (`builtin-providers.ts:11-19`).

**Credentials**
- Flue does not resolve keys itself: "Provider auth (API keys, OAuth) resolves through each provider's own `auth` declaration — env vars on Node, and on Cloudflare via `nodejs_compat`'s `process.env`" (`providers.ts:13-18`).
- pi-ai 1.1.0's Anthropic provider tries, in order: a stored credential; `ANTHROPIC_AUTH_TOKEN` (sent as `Authorization: Bearer`); `ANTHROPIC_OAUTH_TOKEN`; `ANTHROPIC_API_KEY`; then workload identity federation (`ANTHROPIC_FEDERATION_RULE_ID`, `ANTHROPIC_ORGANIZATION_ID`, `ANTHROPIC_IDENTITY_TOKEN_FILE`, plus optional service account and workspace ids). It also declares an OAuth login for Claude Pro/Max subscriptions (`@earendil-works/pi-ai@1.1.0` `dist/providers/anthropic.js:7-75`).

**`useModel` semantics** (`hooks/use-model.ts:17-63`)
- `useModel(model, { thinkingLevel?, compaction? })`. It is **required** and called **exactly once per render**. Calling it twice throws; calling it in a subagent render throws, because a delegate declares its model on `useSubagent()`. Unknown option keys throw.
- "The ARGUMENT may vary render to render (pick a model from durable state); the CALL may not disappear."
- Values are **submission-scoped**: "the runtime reads them when a submission starts, so a different value computed by a later render takes effect on the next submission, not mid-run" (`:25-27`).
- `thinkingLevel` is one of `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` (`agent-tuning.ts:9-17`).
- Per-call overrides: `session.prompt/skill/task({ model?, thinkingLevel? })` (`types.ts:923-933`). Precedence is call-level, then agent default, then `'medium'` for thinking (`session.ts:3826-3840`). Subagents and compaction can each name their own model (`types.ts:420-427, 492-501`).
- `maxTokens` and `temperature` are **not** `useModel` options. They show up only as telemetry fields of the stream options (`types.ts:1170-1181`, `session.ts:2118-2135`).

**Implications for keelrs**
- Flue's and Rig's resolvers overlap heavily: both split at the first `/`, both use a catalog with a fallback for unknown ids (Flue synthesizes a zero-metadata model; Rig connects unlisted ids on a known vendor), both put provider-specific auth in the provider, and both let the user override a provider by id (Flue `setProvider`, Rig `Connector::provide`).
- They differ in a few places:
  - Rig matches dated snapshots (`-20YYMMDD`) but has **no** `-`/`.` version-separator alias.
  - Rig's Anthropic credential reads only `ANTHROPIC_API_KEY` from the env. There is no `ANTHROPIC_AUTH_TOKEN` or OAuth chain unless a `SignIn` is added.
  - Rig has no build-time provider selection. Every vendor in rig-core is compiled in; only the companion-crate vendors are opt-in features.
- Flue's "submission-scoped" rule answers when a changed `use_model` value takes effect: at the next submission, not mid-run. Rig can switch at every model call (§3), so keelrs has to choose its own scope.

---

## 7. Summary table

| Concern | Rig 0.45 | Flue |
|---|---|---|
| String format | `vendor/model`, split at first `/` (`catalog/mod.rs:418`) | `provider-id/model-id`, split at first `/` (`providers.ts:196-205`) |
| Resolver | `Catalog::resolve` / `connect` / `Connector::connect` | `resolveModel` |
| Unknown model id | connects if vendor known; no catalog facts | synthesized zero-metadata model, only if provider opts in; else error |
| Fuzzy match | dated snapshot suffix | `-` vs `.` version separator |
| Provider override | `Connector::provide(vendor, ..)`, replaces | `setProvider(provider)`, replaces; built-ins skip if present |
| Custom vendor | `ProviderId::declare` + `ModelSpec` insert | `createProvider(...)` + `setProvider` |
| Credentials | dialect env vars, first non-empty; `ConnectOptions::api_key`; `SignIn` fallback | provider `auth` declaration (env chain + OAuth) |
| Model handle | `DynModel<Completion>` (object-safe, `Clone`, `'static` futures) | pi-ai `Model<Api>` |
| Per-call model | `request.model` (same wire); `on_model_select` label (rig-agent) | `prompt({ model })`; `useModel` per submission |
| Thinking | `Reasoning::{Off, Effort, Budget}` | `thinkingLevel` (7 levels) |
| max tokens / temperature | request fields; catalog default / refusal | not user-facing on `useModel` |
| Streaming | `DynModel::stream` → `Streamed<Completion>` | pi-ai stream |
