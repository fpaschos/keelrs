# Rebuilding Flue in Rust — Conversation Record

Working notes from a design conversation about recreating the
[Flue](https://github.com/withastro/flue) agent harness in Rust (this repo: `keelrs`).
The conversation went from "what is Flue" → a formalized MVP → a series of follow-up
questions about protocols and integrations.

> Claims about third-party projects (issue numbers, dates, crate names) came from the
> assistant in the original chat and have **not** been re-verified here. See
> [§ 11 Open items and gaps](#11-open-items-and-gaps).

---

## Table of contents

1. [Q1 — What is Flue? (harness and flow)](#1-q1--what-is-flue-harness-and-flow)
2. [Q2 — Can rig.rs replace pi for the core?](#2-q2--can-rigrs-replace-pi-for-the-core)
3. [Q3 — Recreate Flue (sandbox + tools + same API): what's next?](#3-q3--recreate-flue-sandbox--tools--same-api-whats-next)
4. [Q4 — Do I need WASM/TypeScript to declare flows? Runtime and parallelism?](#4-q4--do-i-need-wasm--typescript-to-declare-flows-runtime-and-parallelism)
5. [Q5 — Existing protocols: MCP, skills, ACP, search…](#5-q5--existing-protocols-mcp-skills-acp-search)
6. [Q6 — Flue and AG-UI](#6-q6--flue-and-ag-ui)
7. [Q7 — Does AG-UI support Flue now?](#7-q7--does-ag-ui-support-flue-now)
8. [Q8 — AG-UI links](#8-q8--ag-ui-links)
9. [Q9 — Flue and A2A](#9-q9--flue-and-a2a)
10. [The MVP (consolidated)](#10-the-mvp-consolidated)
11. [Open items and gaps](#11-open-items-and-gaps)

---

## 1. Q1 — What is Flue? (harness and flow)

**Question:** Explain the main idea of the harness and its flow to someone who has
not read the project.

**Main idea.** Flue is a TypeScript framework (from the Astro team) for building
autonomous agents. The LLM is the brain. The **harness** is everything around it
that lets it do real work: tools, a sandbox, memory and sessions, and delegation.
You can think of it as a programmable Claude Code or Codex environment that doesn't
need a terminal UI.

**The harness manages:**

| Concern | Role |
|---|---|
| Model | Which LLM reasons |
| Instructions / skills | What the agent knows and how it behaves |
| Tools | What actions it can take |
| Sandbox | Where code and commands execute |
| Sessions | Conversation and execution context |
| Runtime | Runs the loop, handles tool calls, produces results |

**Flow:**

```
Trigger (API / CLI / GitHub event / message)
  → Harness loads instructions, skills, tools, sandbox, session
  → LLM decides next action
  → Tool executes (shell / API / files) inside the sandbox
  → Result returns to the model → repeat
  → Final result (answer, code changes, completed task)
```

The core is a repeated **reason → act loop**, not a fixed pipeline.

**API shape.** An agent is a function, and hooks declare its capabilities:

```ts
'use agent';

export function Triage() {
  useModel('anthropic/claude-sonnet-4-6');
  useSandbox(local());
  useSkill(triage);
  useTool(searchCode);
  return 'Investigate the bug and attempt a fix.';
}
```

The agent function **re-runs before every model turn**, so instructions and tools can
change as state changes.

**Key insight:** Intelligence (LLM) + Capabilities (harness) + Knowledge (context) are
separate concerns. That separation is what makes the infrastructure reusable.

Flue's conceptual model: **Agent → Harness → Session → Operation → Turn**.

---

## 2. Q2 — Can rig.rs replace pi for the core?

**Question:** I already use and extend Flue. I want to recreate a minimal version in
Rust with the same idea and API. Can I build the pi core on rig.rs instead of pi?

**Answer: yes, with a split.**

- **Rig** handles model providers, messages, tool schemas, model calls, and streaming.
- **Your own core** handles agent lifecycle, the execution loop, state, context
  building, hooks, tool orchestration, and the sandbox.

Use Rig's low-level `CompletionModel` / `CompletionRequest`, **not** `rig::agent::Agent`.
Owning the loop keeps Rig's orchestration decisions from becoming hidden dependencies.

| Capability | Rig provides | Your core |
|---|---|---|
| Model providers | Yes | Adapter |
| Tool schemas | Yes | Registry |
| Tool execution | Yes | Execution policy |
| Agent loop | Yes | **Own implementation** |
| Conversation state | Supported | Own state model |
| Streaming | Yes | Normalize events |
| Steering / follow-ups | Hooks exist | Own queue semantics |
| Abort / cancellation | Runtime support | Own lifecycle |
| Context transformation | Supported | Own pipeline |
| Session persistence | Via integrations | Later |

Pi core owns the stateful agent, events, tool execution, and steering/follow-up
queues. Persistence and compaction live in higher layers (pi's coding-agent harness).

**Core types (sketch):**

```rust
pub struct Agent {
    pub state: AgentState,
    pub tools: ToolRegistry,
    pub model: Box<dyn ModelBackend>,
}

pub struct AgentState {
    pub messages: Vec<Message>,
    pub status: AgentStatus,
    pub turn: usize,
}

pub trait ModelBackend: Send + Sync { /* stream one model response */ }
pub trait ToolExecutor: Send + Sync { /* execute a tool call */ }

pub async fn run_agent_loop(agent: &mut Agent, input: Message) -> Result<(), AgentError> {
    // 1. append input  2. build context  3. call model  4. emit events
    // 5. execute tools  6. append results  7. repeat until done
    todo!()
}
```

**Decision:** Option A. Rig is the model layer only, and we own the pi-style loop.
(Option B would use Rig's agent runner. It's less code but couples us to Rig's lifecycle.)

---

## 3. Q3 — Recreate Flue (sandbox + tools + same API): what's next?

**Target architecture:**

```
Flue-compatible API   (agent defs · useModel · useTool · useSandbox · useSkill)
        │
Rust harness          (lifecycle · hooks & context · sessions · events · tool registry · permissions)
        │
Pi-like core          (reasoning loop · tools · state)
   │            │
Sandbox runtime     Rig.rs
(local/docker/remote)  (providers · inference · streaming)
```

The sandbox is a **first-class runtime component**, not a tool detail. In Flue, the
sandbox supplies the standard file and shell tools, and application tools are
registered separately.

**Build order:**

| Phase | Component | Goal |
|---|---|---|
| 1 | Pi-like agent core | model → tool → model loop, state, events |
| 2 | Sandbox interface | filesystem + command execution |
| 3 | Built-in tools | read, write, edit, bash, grep, glob |
| 4 | Flue-style API | useModel, useSandbox, useTool, useSkill |
| 5 | Sessions and hooks | persistent state, dynamic tool registration |
| 6 | Advanced runtime | subagents, MCP, durability, observability |

**Proposed Rust-native API:**

```rust
fn coding_agent(ctx: &mut AgentContext) -> String {
    ctx.use_model("anthropic/claude-sonnet-4-6");
    ctx.use_sandbox(Sandbox::docker("rust:latest"));
    ctx.use_tool(my_custom_tool());
    ctx.use_skill("skills/coding/SKILL.md");
    "You are a coding agent. Inspect and modify files.".into()
}
```

**Sandbox contract (minimum):**

```rust
#[async_trait::async_trait]
pub trait Sandbox: Send + Sync {
    async fn read(&self, path: &str) -> Result<Vec<u8>, Error>;
    async fn write(&self, path: &str, content: &[u8]) -> Result<(), Error>;
    async fn exec(&self, command: &str) -> Result<ExecResult, Error>;
}
```

Extend it later with directory listing, cancellation, env vars, and lifecycle.
Implementations: `InMemorySandbox` (tests) → `LocalSandbox` (trusted dev) →
`DockerSandbox` (isolation, resource limits, restricted network, timeouts).
**A local process runner is not a security boundary.** Keep execution policy
separate from the sandbox trait.

**Loop with hooks:**

```
run(input)
 ├─ load session
 ├─ evaluate agent function (useModel / useSandbox / useTool / useSkill)
 ├─ build model context
 ├─ call Rig completion model
 ├─ tool calls? ── yes → validate, execute, append results, repeat
 │              └─ no  → finish
 ├─ persist state
 └─ emit completion event
```

**First acceptance test:** *"Create hello.txt containing Hello World, then read it
back."* It runs against the in-memory sandbox and proves the loop, schemas, dispatch,
sandbox state, and conversation all work together. After that, swap in Docker with no
API change.

---

## 4. Q4 — Do I need WASM / TypeScript to declare flows? Runtime and parallelism?

**No WASM needed.**

| Layer | Choice |
|---|---|
| Developer-facing API | TypeScript (Flue-style hooks) |
| Runtime, scheduling, tools, sandbox | Rust |
| Models | Rig.rs |
| Concurrency | Tokio |
| WASM | Optional, later: an execution env for untrusted plugins |

**TS ↔ Rust bridge options:**

| Approach | Pros | Cons |
|---|---|---|
| N-API (napi-rs) | Fast, native, familiar | Native binaries, binding complexity |
| **IPC to a Rust process** | Crash isolation, easy debugging | Serialization, process management |
| JS engine embedded in Rust | One process, Rust owns lifecycle | Node API / npm support is hard |

**Recommendation for v1:** a long-lived Rust process talking JSON-RPC over stdio.
Move to N-API or a binary protocol only if measurements justify it.

The TS side evaluates the agent function and sends a declarative config to Rust:

```json
{
  "agent": "ResearchAgent",
  "model": "anthropic/claude-sonnet-4-6",
  "sandbox": { "type": "docker", "image": "node:22" },
  "tools": ["searchWeb"],
  "instructions": "Research the topic and write a report."
}
```

JS keeps the actual `searchWeb` function, and Rust keeps its id and JSON schema. When
the model calls it, Rust sends the invocation back to JS and receives the result. The
TS function is **re-evaluated before each model call**, which keeps Flue's dynamic
hook semantics.

**Three kinds of parallelism (keep them separate):**

1. **Parallel agents.** One Tokio task per independent session.
   ```rust
   let a = tokio::spawn(run_agent(session_a));
   let b = tokio::spawn(run_agent(session_b));
   let (ra, rb) = tokio::try_join!(a, b)?;
   ```
2. **Parallel tools within a turn.** Each tool declares a policy:
   ```rust
   enum ToolConcurrency { Parallel, Exclusive }
   ```
   Read-only tools run in parallel and conflicting mutations run serially. Preserve
   tool-call ids and return results in a stable order.
3. **Parallel subagents.** A parent spawns children with their own contexts and awaits
   them. This needs cancellation propagation, a maximum depth, concurrency limits, and
   token budgets.

**WASM caveats:** running TS inside WASM is not automatic, because it needs a JS engine
compiled to WASM. WASM is also not a Docker equivalent unless host capabilities are
restricted correctly. Use Docker for shell and filesystem first, and consider a
Wasmtime/WASI backend later for lightweight plugins.

**Proposed repo layout:**

```
keelrs/
├── crates/
│   ├── core/            # loop, state, events
│   ├── rig-adapter/     # model integration
│   ├── scheduler/       # tokio concurrency
│   ├── sandbox/         # sandbox trait
│   ├── sandbox-docker/
│   ├── tools/           # built-in tools
│   ├── protocol/        # TS <-> Rust messages
│   └── runtime/         # orchestration
├── packages/
│   ├── runtime/         # TS hooks API
│   └── cli/
└── examples/coding-agent/
```

Start with one crate and one TS package, and split them once the boundaries are proven.

**Foundation:** make a **durable agent session** the foundation, not a generic DAG
workflow engine. Parallel operations and subagents live inside sessions.

**Milestone:** two TS-defined agents run concurrently in Rust, each using a
sandbox-backed tool, with independent event streams and cancellation.

---

## 5. Q5 — Existing protocols: MCP, skills, ACP, search…

**Question:** What about existing protocols like "cap", MCP, skills, and search? Do I
need them all?

| Standard | Purpose | Priority |
|---|---|---|
| **MCP** | External tools, resources, prompts | **P0** |
| **Agent Skills** (`SKILL.md`) | Portable instructions, scripts, expertise | **P0** |
| **AGENTS.md** | Repo-level instructions for coding agents | **P0** (coding) |
| ACP (Zed Agent Client Protocol) | Editor/IDE ↔ agent | P1 |
| OpenTelemetry | Tracing and observability | P1 |
| A2A | Remote agent ↔ agent | P2 |
| AG-UI | Agent events → web frontends | P2 |
| WASI / WASM | Constrained component execution | Optional |

*Note:* "ACP" can mean Zed's Agent Client Protocol (editors) or IBM's Agent
Communication Protocol, which has since merged into A2A.

**Principle:** the internal runtime depends on **no** external protocol. MCP, native
Rust, TS, and sandbox tools all become one internal `Tool`. CLI, ACP, and AG-UI all
consume one internal event stream.

```rust
pub enum ToolSource { Native, TypeScript, Mcp, Sandbox }

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub source: ToolSource,
}

pub enum AgentEvent {
    TurnStarted,
    TextDelta(String),
    ToolCallStarted { id: String, name: String },
    ToolCallCompleted { id: String },
    TurnCompleted,
    AgentCompleted,
}
```

```
ACP client  ─┐
AG-UI front ─┤
CLI         ─┼──► Rust agent runtime ──► native / MCP / sandbox / TS tools
TS SDK      ─┘
```

**MCP:** client over stdio and Streamable HTTP, with tool discovery and invocation,
resources and prompts, per-session authorization, and cancellation, timeouts, and
connection lifecycle. The runtime, not MCP, decides concurrency and permissions.

**Skills:** use the open `SKILL.md` format with progressive disclosure: load the name
and description at discovery and the full body on demand. Skills **do not grant
permissions**.

```
skills/
  code-review/{SKILL.md, references/, scripts/}
  testing/SKILL.md
```

**Reusable Rust crates:**

| Component | Crate / spec |
|---|---|
| LLM | `rig` |
| MCP | `rmcp` (official Rust MCP SDK) |
| Async | `tokio` |
| Docker | `bollard` |
| WASM | `wasmtime` |
| Telemetry | `opentelemetry` |
| Skills / ACP / A2A | respective specs and SDKs |

**Roadmap:**

1. **Core harness.** Loop, Rig adapter, Tokio, events, sandbox file and shell tools.
2. **MCP + Skills.** `rmcp` client, `SKILL.md` loader, `AGENTS.md`, unified registry.
3. **Flue-compatible TS API.** Hook evaluation, IPC bridge, `useModel`/`useSandbox`/`useTool`/`useSkill`/`useMcp`.
4. **Parallel + durable.** Concurrent agents, subagents, cancellation, persisted sessions, checkpoints, recovery.
5. **External compat.** ACP, AG-UI, A2A, and optionally exposing an MCP server.

**Target TS API:**

```ts
'use agent';

export function CodingAgent() {
  useModel('anthropic/claude-sonnet-4-6');
  useSandbox(docker({ image: 'node:22', network: 'restricted' }));
  useSkill('./skills/coding');
  useMcp({ name: 'github', transport: 'stdio', command: 'github-mcp-server' });
  useTool(customTool);
  return 'You are an autonomous coding agent.';
}
```

**Own these yourself:** the agent loop, session model, sandbox contract, tool registry,
and event schema. Everything else is an adapter to an existing standard.

---

## 6. Q6 — Flue and AG-UI

Flue is the harness. AG-UI is the agent ↔ frontend protocol for streaming text, tool
activity, and shared state. Keep the internal event type and add an **AG-UI adapter**
that translates it.

| Harness event | AG-UI event |
|---|---|
| Operation begins | `RUN_STARTED` |
| Model starts message | `TEXT_MESSAGE_START` |
| Token stream | `TEXT_MESSAGE_CONTENT` |
| Message done | `TEXT_MESSAGE_END` |
| Tool requested | `TOOL_CALL_START` / `TOOL_CALL_ARGS` |
| Args complete | `TOOL_CALL_END` |
| Tool returned | `TOOL_CALL_RESULT` |
| UI state change | `STATE_DELTA` |
| Operation done | `RUN_FINISHED` |
| Operation failed | `RUN_ERROR` |

A Flue **session** contains many **operations**. An AG-UI **run** is one execution.
**Don't equate session with run.**

```rust
pub struct AgUiAdapter { /* internal events → AG-UI events */ }

impl AgUiAdapter {
    pub fn translate(&self, event: HarnessEvent) -> Vec<AgUiEvent> { todo!() }
}
```

Expose it over Axum + SSE. Cancellation and approvals are separate inbound endpoints.
The `ag-ui-protocol` crate on docs.rs has Rust event types, so check it against
AG-UI 1.0 before adopting it.

Core requirements that AG-UI needs from day one: stable run, session, and tool-call
ids, ordered events, cancellation, and resumable state.

---

## 7. Q7 — Does AG-UI support Flue now?

**No** (as of 2026-10-25, per the chat). The plan is tracked in ag-ui issue **#2793**,
"Flue framework integration". It is open with 0 of 5 tasks done and no linked PRs, and
Flue isn't on AG-UI's list of supported integrations. For keelrs, AG-UI compatibility
is something **we implement**.

## 8. Q8 — AG-UI links

- Website / docs: <https://docs.ag-ui.com>
- GitHub: <https://github.com/ag-ui-protocol/ag-ui>
- Flue issue: <https://github.com/ag-ui-protocol/ag-ui/issues/2793>

## 9. Q9 — Flue and A2A

There's no verified native A2A support in Flue. Flue documents MCP, tools, sessions,
and execution. A2A can be integrated through a custom adapter:

```
TS API → Rust core → A2A adapter ─┬─ A2A client → remote agents
                                  └─ A2A server → expose our agents
```

Use the official A2A Rust SDK instead of hand-rolling the protocol. Do this **after**
the runtime and sessions work. It isn't needed for local parallel subagents.

- Flue: <https://github.com/withastro/flue>
- A2A: <https://a2a-protocol.org>

---

## 10. The MVP (consolidated)

The chat described the MVP piece by piece. Here it is in one place.

**In scope:**

- One Rust crate (`keelrs`).
- A Rig-backed `ModelBackend` using one provider and low-level completion calls.
- A pi-style loop: model → tool calls → results → model, until no tool calls remain.
- In-memory `AgentState`: messages, status, turn count.
- `ToolRegistry` with JSON schemas and a `Parallel` / `Exclusive` policy.
- `Sandbox` trait with an `InMemorySandbox`, then `LocalSandbox`.
- Built-in tools: `read`, `write`, `bash` (then `edit`, `grep`, `glob`).
- A typed `AgentEvent` stream.
- A Rust-native hook API (`use_model`, `use_sandbox`, `use_tool`), re-evaluated each turn.

**Out of scope for the MVP:** persistence, compaction, skills, MCP, Docker, TS bridge,
subagents, AG-UI, ACP, and A2A.

**Acceptance tests:**

1. *"Create hello.txt containing Hello World, then read it back."* Passes against
   `InMemorySandbox`.
2. *(Next milestone)* Two agents run concurrently on Tokio with independent event
   streams, and cancelling one leaves the other unaffected.

---

## 11. Open items and gaps

Reviewing the whole conversation turned up these unresolved or missing pieces.

### Questions asked but never answered

- **"cap" protocol.** The Q5 question mentions "cap". The answer covered ACP but never
  said what "cap" meant. It might be a typo for ACP, a reference to a
  capability-based security model, or something else. **To decide.**
- **"search".** Q5 also asked about search, and it was never addressed. Possible
  meanings:
  - code search tools (`grep` / `glob`, maybe ripgrep via the `grep` crate),
  - a web search tool (native, or through an MCP server such as Brave or Exa),
  - skill/tool discovery search when there are many skills or MCP tools.
- **Exact Flue TS API, or a Rust-native API with the same concepts?** Asked in Q3 and
  never answered. The MVP above assumes Rust-native first and the TS layer later.
- **Standalone Rust daemon, or embedded in Node?** Asked in Q4 and never answered. The
  recommendation was a standalone process with JSON-RPC over stdio.

### Design topics not yet covered

- **Mapping Flue's model to Rust types.** Agent → Harness → Session → Operation → Turn
  was named but never turned into structs. `AgentState` currently mixes session and
  turn.
- **Steering and follow-up queues** (pi core semantics): listed but not designed.
- **Context management:** context-window limits, compaction or summarization, and
  token counting.
- **Permissions and approvals:** human-in-the-loop tool approval and the policy model.
  Skills and MCP explicitly don't grant permissions, so what does?
- **Session persistence format** and recovery: where state lives and how a crashed
  operation resumes.
- **Error handling:** model retries, tool errors fed back to the model, max-turn
  limits.
- **Testing strategy:** a scripted or mock `ModelBackend` so the loop can be tested
  without a real LLM.
- **Sandbox trait vs. tool list mismatch:** the trait has `read`/`write`/`exec`, but
  `edit`/`grep`/`glob` need `list_dir`, `stat`, or an exec-based implementation.
- **Cost of per-turn re-evaluation over IPC:** re-running the TS agent function every
  turn means one IPC round-trip per turn. That's fine, but the protocol should account
  for it.
- **Secrets and credentials** for providers and sandboxed tools.
- **Structured output** and typed results from an operation.

### Facts to re-verify

- AG-UI issue #2793 and its status. Other details from the chat (the Sept 18, 2026
  date and 0/5 tasks) are also unconfirmed.
- The name and maintenance status of the official A2A Rust SDK, and whether the
  `ag-ui-protocol` crate supports AG-UI 1.0.
- Whether Rig's current API still exposes raw `CompletionRequest` with manual tool
  execution.
- The model ids in the examples (`claude-sonnet-4-6`) are placeholders. Use a current
  model.

### Note about the source

The pasted conversation ends mid-sentence (*"now i need to create a md fi…"*), so
anything after that point was not captured.
