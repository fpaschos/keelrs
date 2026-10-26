Read flue project I want the main idea of the harness and flow something that it has not read the project GitHub

I can read the Flue project on GitHub and explain its main idea, especially the harness architecture and execution flow, in a way that makes sense even to someone who hasn't read the project.

Please send me the GitHub repository link.

I'll focus on:

    Main idea — What Flue is trying to build and what problem it solves.

    Harness — What the harness does, its main components, and how they work together.

    Execution flow — How a request enters the system, gets processed, uses tools, and produces a result.

    Architecture — How the important modules connect.

    Simple example — Walk through one complete execution from start to finish.

I'll explain the concepts first, rather than just repeating the README, and distinguish what the code actually implements from what the documentation describes.
Search

I found the project you're referring to:

Flue — The Agent Harness Framework

GitHub: withastro/flue | Official agent documentation
1. The main idea

Flue is a TypeScript framework for building autonomous AI agents, created by the team behind Astro.

Its central idea is:

An LLM is the brain, but the harness is the system that gives that brain the ability to do real work.

Instead of building a chatbot that only responds with text, Flue lets developers build agents that can use tools, execute commands, modify files, remember context, and delegate tasks.

Think of it as a programmable version of the environment that makes Claude Code or Codex useful, without requiring a human-operated terminal interface.
GitHub
+1
2. What is the harness?

The harness is the execution environment around the LLM. It manages:

    Model: Which LLM does the reasoning.

    Instructions and skills: What the agent knows and how it should behave.

    Tools: Which actions the agent can perform.

    Sandbox: Where code and commands execute.

    Sessions: How conversation and execution context are maintained.

    Runtime: How the agent runs, handles tool calls, and produces results.

The important distinction is that Flue doesn't replace the LLM. It makes the LLM operational.
3. The basic execution flow

User request / Trigger
API, CLI, GitHub event, or message

Flue Harness
Load instructions, skills, tools, sandbox, session

LLM reasoning
Decides the next action

Tool execution
Shell, API, files

Sandbox
Isolated workspace

Results return to the model; repeat as needed

Final result
Answer, code changes, or completed task

The key is the repeated reasoning-and-action loop, not a fixed sequence of predefined steps.
4. What makes Flue different?

The interesting architectural choice is that the agent itself is a function, and hooks configure its capabilities.

For example:

'use agent';

export function Triage() {
useModel('anthropic/claude-sonnet-4-6');
useSandbox(local());
useSkill(triage);
useTool(searchCode);

return 'Investigate the bug and attempt a fix.';
}

Flue registers this function as an agent. When the agent runs, its function provides instructions and capabilities to the runtime.

One particularly interesting detail: the agent function re-runs before each model turn, so its instructions can adapt to the current state.
Flue
+1
5. A real example

Imagine someone asks:

"Find the bug in my GitHub project and fix it."

The flow would be:

    Flue receives the task and identifies the agent session.

    The harness initializes the model, sandbox, tools, and instructions.

    The model decides to inspect the code.

    The harness executes the requested tool and returns its result.

    The model analyzes the result and may request more tools.

    This continues until the model completes the task or stops.

    Flue records the conversation and makes the result available.

Flue also supports persistence and recovery mechanisms, so accepted work can survive certain interruptions.
Flue
+1
6. The most important insight

Flue separates three concerns:

Intelligence (LLM) + Capabilities (Harness) + Knowledge (Context).

That separation is what allows developers to reuse agent infrastructure instead of writing a custom execution loop, tool dispatcher, sandbox integration, and persistence layer for every new agent.

This explanation is based on the project's GitHub README and official architecture documentation, rather than a complete line-by-line source audit.

If you're studying how to build your own harness, the most valuable next step is examining Flue's actual runtime internals: how the model loop invokes tools, how sessions are stored, and how sandbox execution is controlled.
Ok I already use it and also I have extend it but I want to recreate something minimal and n the beginning with the same idea and api with rust first question instead of pi use rig.rs can I do the pi core like that

Yes. You can use rig.rs instead of pi to build a minimal Flue-like harness in Rust. But there's an important architectural distinction:

Rig can replace much of Pi's LLM interaction layer, but Rig is not necessarily a drop-in replacement for all of Pi's agent runtime behavior.

Since you want to recreate Flue from the beginning, with the same underlying ideas and a similar API, I would separate the system into two layers:

    Rig: Model providers, messages, tool definitions, and model calls.

    Your own core: Agent lifecycle, execution loop, sessions, context management, hooks, tool orchestration, and eventually sandboxing.

This gives you control over the harness without having to implement provider integrations yourself.
1. The architecture I would build

Flue-like API
Agent definitions, hooks, configuration
Your Rust Harness Core

Agent state

Execution loop

Tool registry

Event stream

Context builder

Turn control

Rig Model Adapter
Provider APIs, message conversion, streaming

LLM Providers
Anthropic · OpenAI · Gemini · Ollama

I would use Rig's lower-level CompletionModel / CompletionRequest APIs rather than delegating the entire lifecycle to rig::agent::Agent.

Rig already offers its own agent loop, tool execution, and hooks. But if your goal is to reproduce Pi-core semantics, owning the loop prevents Rig's orchestration decisions from becoming hidden dependencies.
Rig
+1
2. Pi core vs Rig

Capability


Rig provides


Your Rust core

Model providers


Yes


Adapter

Tool schemas


Yes


Registry

Tool execution


Yes


Execution policy

Agent loop


Yes


Own implementation

Conversation state


Supported


Own state model

Streaming


Yes


Normalize events

Steering / follow-ups


Hooks exist


Own queue semantics

Abort / cancellation


Runtime support


Own lifecycle

Context transformation


Supported


Own context pipeline

Session persistence


Supported via integrations


Later

Pi's core specifically manages the stateful agent, events, tool execution, and steering/follow-up queues. Persistence and compaction belong to higher layers of Pi's coding-agent harness.
GitHub
+1
3. Minimal Rust core

I would start with these internal types, independent of Rig:

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

pub trait ModelBackend: Send + Sync {
// Stream one model response
}

pub trait ToolExecutor: Send + Sync {
// Execute a tool call
}

Then define one central execution function:

pub async fn run_agent_loop(
agent: &mut Agent,
input: Message,
) -> Result<(), AgentError> {
// 1. Append user input
// 2. Build model context
// 3. Request model response
// 4. Emit streaming events
// 5. Execute requested tools
// 6. Append tool results
// 7. Repeat until complete
todo!()
}

This is a design sketch, not yet a compilable implementation.
4. What I would implement first

For a minimal first version, I would implement only:

    One model backend powered by Rig.

    One agent with an in-memory message history.

    A model → tools → model execution loop.

    A small tool registry.

    A typed event stream for observing execution.

No persistence, sandbox, skills, compaction, or multi-agent orchestration yet.

Then build the Flue-like declarative API on top of this core, rather than mixing the API and runtime.
5. One architectural decision matters most

There are two valid approaches:

A. Rig as the model layer

You own the Pi-style loop, state, tool execution, and events. More control and a cleaner independent harness.

B. Rig as the complete agent core

Use Rig's built-in agent runner and build Flue-like APIs around it. Less initial code, but tighter coupling to Rig's lifecycle.

I recommend A for your goal of recreating Flue's fundamental architecture from scratch. It is the closest conceptual replacement for Pi core, while Rig handles the model-specific complexity.
I want to recreate flueand sandboxing and tools with same api. Or pi so what is mext

If your goal is to recreate Flue in Rust, including its API, sandboxing, tools, and Pi-style agent execution, then the next step is to define the architecture before implementing anything.

I would build a Flue-compatible harness powered by a Rust core, rather than simply wrapping Rig's agent.

The key decision is:

Use Rig for model integration, but implement the Pi-like agent core yourself.

That way you can reproduce Flue's behavior without depending on Pi internally.
1. Target architecture

Flue-compatible API
Agent definitions · useModel · useTool · useSandbox · useSkill
Rust Harness

Agent lifecycle

Hooks & context

Sessions

Event stream

Tool registry

Permissions

Pi-like Core
Reasoning loop, tools, state

Sandbox Runtime
Local, Docker, remote

Rig.rs
Model providers · inference · streaming

You should treat the sandbox as a first-class runtime component, not as a tool-specific implementation detail.
2. What to build next, in order

Phase


Component


Goal

1


Pi-like agent core


Model → tool → model loop, state, events

2


Sandbox interface


Filesystem and command execution

3


Built-in tools


read, write, edit, bash, grep, glob

4


Flue-style API


useModel, useSandbox, useTool, useSkill

5


Sessions and hooks


Persistent state, dynamic tool registration

6


Advanced runtime


Subagents, MCP, durability, observability

This order matters because Flue's sandbox supplies the standard file and shell tools, while application tools are registered separately.
Flue
+1
3. The first milestone should be a working vertical slice

Instead of implementing six independent crates immediately, build a single Rust crate that can:

    Accept an agent definition.

    Create a sandbox.

    Expose sandbox-backed tools to the model.

    Execute model-requested tool calls.

    Stream events.

    Return the final response.

For example, your eventual Rust API could look like:

fn coding_agent(ctx: &mut AgentContext) -> String {
ctx.use_model("anthropic/claude-sonnet-4-6");
ctx.use_sandbox(Sandbox::docker("rust:latest"));
ctx.use_tool(my_custom_tool());
ctx.use_skill("skills/coding/SKILL.md");

    "You are a coding agent. Inspect and modify files.".into()
}

This is a proposed API, not existing Flue or Rig syntax.

Importantly, the function should run again before each model turn, matching Flue's dynamic hook semantics.
Flue
4. Define the sandbox contract first

#[async_trait::async_trait]
pub trait Sandbox: Send + Sync {
async fn read(&self, path: &str) -> Result<Vec<u8>, Error>;

    async fn write(
        &self,
        path: &str,
        content: &[u8],
    ) -> Result<(), Error>;

    async fn exec(
        &self,
        command: &str,
    ) -> Result<ExecResult, Error>;
}

This is the minimum interface sketch. You can extend it with directory listing, process cancellation, environment variables, and lifecycle methods.

Build two implementations eventually:

    LocalSandbox for trusted development environments.

    DockerSandbox for isolated execution with resource limits, restricted networking, filesystem mounts, and command timeouts.

A local process runner is not a security boundary. Keep the execution policy separate from the sandbox interface.
5. Use Rig underneath the loop

Rig explicitly supports raw completion requests and manually executing returned tool calls. That is the integration point you need.
Rig

The core loop becomes:
run(input)
|
+-- load session
|
+-- evaluate agent function
|     +-- useModel
|     +-- useSandbox
|     +-- useTool
|     +-- useSkill
|
+-- build model context
|
+-- call Rig completion model
|
+-- model requests tools?
|     |
|     +-- yes: validate and execute
|     |         |
|     |         +-- append results
|     |         +-- repeat
|     |
|     +-- no: finish
|
+-- persist state
+-- emit completion event
6. The immediate next task

Implement Phase 1 and Phase 2 together, but only with an in-memory sandbox initially.

The first acceptance test should be:

    Give the agent the instruction "Create hello.txt containing Hello World, then read it back."

A successful run proves that your agent loop, tool schemas, tool dispatch, sandbox state, and model conversation all work together.

After that, replace the in-memory sandbox with Docker without changing the agent API.

One question before we design the actual repository:

Do you want to preserve Flue's TypeScript API exactly, with Rust running underneath, or create a Rust-native API with the same concepts?

That determines whether we need a TypeScript-to-Rust bridge or can implement the entire harness as a native Rust library.
Do I need wasmand typescript integration to declare the flows what about the runtime nd parallelisation

No, you don't need WebAssembly to declare Flue-style flows in TypeScript. And I would not introduce WASM at the beginning.

For your project, I recommend:

    TypeScript for the developer-facing Flue API.

    Rust for the agent runtime, scheduling, parallel execution, tools, and sandbox management.

    Rig.rs for model integration.

    Tokio for asynchronous execution and concurrency.

    WASM as an optional future execution environment for untrusted plugins, not the primary way to run agent definitions.

The most important architectural decision is how TypeScript agent definitions communicate with your Rust runtime.
1. Recommended architecture

TypeScript SDK
useModel · useTool · useSandbox · useSkill · agent()

JS Runtime (Node.js initially)
Evaluates agent functions and hooks
Rust Runtime (Tokio)

Agent scheduler

Pi-like execution loop

Session manager

Event stream

Tool dispatcher

Cancellation

Rig.rs
LLM requests and streaming

Sandbox backends
Local · Docker · Remote

This gives you a familiar TypeScript authoring experience while keeping the execution engine native.
2. How TypeScript integrates with Rust

You have three realistic options.

Approach


Advantages


Trade-off

Node.js + Rust via N-API


Fast calls, native integration, familiar TS API


Native binaries and binding complexity

Node.js + Rust process via IPC


Independent runtimes, crash isolation, easy debugging


Serialization and process management

Embedded JS engine in Rust


Single host process, Rust controls lifecycle


More work supporting Node APIs and npm packages

My recommendation for v1: Node.js + Rust via IPC, using a long-lived Rust process.

Use JSON-RPC over stdio initially. Later, if performance measurements justify it, consider N-API or a binary protocol.

For example:

'use agent';

export function ResearchAgent() {
useModel('anthropic/claude-sonnet-4-6');
useSandbox(docker({ image: 'node:22' }));
useTool(searchWeb);

return 'Research the topic and write a report.';
}

The TypeScript runtime evaluates the function and sends a declarative configuration to Rust:

{
"agent": "ResearchAgent",
"model": "anthropic/claude-sonnet-4-6",
"sandbox": {
"type": "docker",
"image": "node:22"
},
"tools": ["searchWeb"],
"instructions": "Research the topic and write a report."
}

The JavaScript process retains the actual searchWeb function. Rust retains its registered tool identifier and JSON schema.

When the model calls searchWeb, Rust sends a tool invocation back to JavaScript and receives the result.

Crucially, the TypeScript function is re-evaluated before each model call, matching Flue's dynamic hook model. Conditional tools and changing instructions therefore remain possible.
Flue
3. How parallelization should work

There are three different kinds of parallelism, and they should not be mixed together.

Tokio Scheduler
Concurrency limits · cancellation · task ownership

Agent A
Session A

Agent B
Session B

Agent C
Session C

Tool execution pools
Concurrent safe tools · serialized conflicting writes

Sandbox manager
Isolated environments and resource quotas
A. Parallel agents

Each independent agent session gets its own asynchronous task.

let a = tokio::spawn(run_agent(session_a));
let b = tokio::spawn(run_agent(session_b));

let (a_result, b_result) = tokio::try_join!(a, b)?;

This allows many sessions to wait on LLM calls or sandbox operations concurrently.
B. Parallel tools within an agent

When a model returns multiple tool calls, you can execute independent tools concurrently.

For example, reading three different files is usually safe to parallelize. Editing the same file from three tools is not.

Give each tool an execution policy:

enum ToolConcurrency {
Parallel,
Exclusive,
}

Start conservatively: parallelize independent read-only tools and serialize mutations that may conflict. Preserve the model's tool-call IDs and return results in a stable order.
C. Parallel subagents

A parent agent can spawn multiple child agents, each with its own context.

For example, a coding agent could delegate test analysis and documentation research simultaneously.

The parent awaits their results and then continues its own model loop.

You will need cancellation propagation, maximum recursion depth, concurrency limits, and token budgets so delegation cannot grow without control.
4. What about WASM?

WASM becomes useful when you want to run untrusted extension code with constrained capabilities.

For example, Wasmtime can host WASI components with explicitly granted filesystem or other host capabilities.

But there are two important caveats:

    Running arbitrary TypeScript inside WASM is not automatic. You would need a JavaScript engine compiled for WASM or a suitable compilation strategy.

    WASM does not automatically provide a Docker-equivalent sandbox, nor does it guarantee safety without correctly restricting host capabilities.

For your first implementation, Docker or another container backend is the simpler choice for sandboxed shell and filesystem tools.

Later, WASM could be a separate backend for lightweight plugins.
5. How I would structure the repository
   my-flue/
   ├── crates/
   │   ├── core/          # Agent loop, state, events
   │   ├── rig-adapter/   # Model integration
   │   ├── scheduler/     # Tokio concurrency
   │   ├── sandbox/       # Sandbox abstraction
   │   ├── sandbox-docker/
   │   ├── tools/         # Built-in tool implementations
   │   ├── protocol/      # TS <-> Rust messages
   │   └── runtime/       # Orchestration
   │
   ├── packages/
   │   ├── runtime/       # TypeScript hooks API
   │   └── cli/           # Developer CLI
   │
   └── examples/
   └── coding-agent/

You can start with one Rust crate and one TypeScript package, then split them once the boundaries are proven.
6. One Flue-specific detail worth preserving

Flue's actual model is Agent → Harness → Session → Operation → Turn. Its repository explicitly distinguishes durable conversations from conventional workflow runs.
GitHub

That matters for your scheduler.

Don't make a generic DAG workflow engine the foundation. Make a durable agent session the foundation, then support parallel operations and subagents within it.

For your first working version, the milestone should be:

Two TypeScript-defined agents executing concurrently in Rust, each using a sandbox-backed tool, with independently streamed events and cancellation.

That single test validates the TypeScript bridge, Rig integration, Pi-like loop, tool execution, sandbox lifecycle, and Tokio scheduler together.

One design question remains: Do you want the Rust runtime to run as a standalone server/daemon, or embedded inside the Node.js process? I would choose a standalone process for v1 because it gives you cleaner isolation and a future path to remote workers.
What about existing protocols like cap and mcp and skills and search all related so I need them

Yes. Since you're rebuilding Flue as a Rust-powered agent harness with a TypeScript API, you should design around existing open protocols instead of inventing everything yourself.

But you do not need to implement all of them. Each protocol solves a different problem, and some belong at the edges of your runtime rather than inside its core.

I searched the current agent protocol ecosystem. Here's what I would adopt.
1. The protocols relevant to your harness

Standard


Purpose


My priority

MCP


Connect agents to external tools, resources, and services


P0 — Essential

Agent Skills


Portable instructions, scripts, and expertise


P0 — Essential

AGENTS.md


Repository-level instructions for coding agents


P0 — Essential for coding

ACP


Connect coding agents to IDEs and editors


P1 — Recommended

A2A


Communication between independent remote agents


P2 — Later

AG-UI


Stream agent events to web frontends


P2 — Later

WASI / WASM


Portable constrained execution of components


Optional

OpenTelemetry


Distributed tracing and observability


P1 — Recommended

MCP, A2A, and Agent Skills are complementary rather than competing standards.
a2a-protocol.org
+2

One terminology clarification: ACP commonly refers to Zed's Agent Client Protocol, for connecting editors to coding agents. IBM also had an Agent Communication Protocol for agent-to-agent communication, which was merged into A2A.
GitHub
2. Where each protocol belongs

Developer interfaces
TypeScript Flue API · CLI · IDE via ACP · Web via AG-UI
Rust Agent Runtime

Pi-style agent loop

Tokio scheduler

Sessions & events

Context builder

Permissions

Tool registry

Context and capabilities
Agent Skills · AGENTS.md · MCP tools and resources

Rig.rs
Models

Sandboxes
Execution

MCP / A2A
External systems

The important idea is that your internal runtime should not depend on any one external protocol.

MCP tools, native Rust tools, and TypeScript tools should all become the same internal Tool representation. Similarly, ACP, CLI, and AG-UI should consume the same underlying agent event stream.
3. MCP: adopt early

MCP is especially useful because you don't want to reimplement every integration.

Your harness should support:

    MCP client connections over stdio and Streamable HTTP.

    Tool discovery and invocation.

    Resources and prompts.

    Tool authorization and per-session access policies.

    Cancellation, timeouts, and connection lifecycle management.

The official specification defines tools, resources, and prompts as distinct capabilities.
GitHub
+1

A good Rust interface:

pub enum ToolSource {
Native,
TypeScript,
Mcp,
Sandbox,
}

pub struct ToolDefinition {
pub name: String,
pub description: String,
pub input_schema: serde_json::Value,
pub source: ToolSource,
}

Your execution loop should not care where a tool comes from. It invokes the tool through a common dispatcher.
4. Skills: adopt the open format

Use the standard SKILL.md format instead of inventing a proprietary skill system.
skills/
code-review/
SKILL.md
references/
scripts/
testing/
SKILL.md

Load skill names and descriptions during discovery, then load full instructions only when needed. This is the progressive-disclosure model described in the Agent Skills specification.
GitHub

Your Flue-style API can remain:

useSkill("./skills/code-review");
useSkill("./skills/testing");

The Rust runtime handles discovery, loading, validation, and sandbox-controlled script execution.

Skills provide instructions and resources; they should not automatically grant extra permissions.
5. Rust libraries you can reuse

You don't need to implement these protocols from scratch.

Component


Existing implementation

LLM models


Rig.rs

MCP client/server


rmcp

Async scheduling


Tokio

Docker management


Bollard

WASM runtime


Wasmtime

Skills specification


Agent Skills

ACP


Agent Client Protocol

A2A


A2A specification

Telemetry


OpenTelemetry

In particular, rmcp is the official Rust MCP SDK, built around Tokio, with client/server support, transport implementations, and tool macros.
GitHub
6. The runtime boundary is more important than the protocol choice

I would define a single internal event and tool system.

pub enum AgentEvent {
TurnStarted,
TextDelta(String),
ToolCallStarted {
id: String,
name: String,
},
ToolCallCompleted {
id: String,
},
TurnCompleted,
AgentCompleted,
}

Then create adapters:
ACP client       ──┐
AG-UI frontend   ──┤
CLI              ──┼──> Rust Agent Runtime
TypeScript SDK   ──┘           |
+── Native tools
+── MCP tools
+── Sandbox tools
+── TypeScript tools

Your runtime controls the concurrency and security of every tool, regardless of the source protocol.

For example, two MCP tools can run concurrently only when your execution policy permits it. MCP itself should not decide your sandbox permissions or file-write concurrency.
7. The implementation roadmap I'd follow

Milestone 1 — Core harness

Rust agent loop, Rig model adapter, Tokio execution, event stream, and sandbox-backed file/shell tools.
Build first

Milestone 2 — MCP + Skills

MCP client with rmcp, SKILL.md loader, AGENTS.md support, and unified tool registry.

Milestone 3 — Flue-compatible TypeScript API

Hook evaluation, IPC bridge, useModel, useSandbox, useTool, useSkill, and useMcp.

Milestone 4 — Parallel and durable execution

Concurrent agents, subagents, cancellation, persisted sessions, checkpoints, and recovery.

Milestone 5 — External compatibility

ACP for editors, AG-UI for frontends, A2A for remote agents, and optional MCP server exposure.
8. What your final TypeScript API could look like

'use agent';

export function CodingAgent() {
useModel('anthropic/claude-sonnet-4-6');

useSandbox(docker({
image: 'node:22',
network: 'restricted',
}));

useSkill('./skills/coding');

useMcp({
name: 'github',
transport: 'stdio',
command: 'github-mcp-server',
});

useTool(customTool);

return 'You are an autonomous coding agent.';
}

This is a proposed API. It preserves Flue's declarative developer experience while letting Rust own execution, scheduling, and sandbox management.

The Rust runtime would use Rig for model calls, rmcp for MCP connections, a skills loader for context, and sandbox backends for isolated tool execution.

My strongest architectural recommendation: make the agent loop, session model, sandbox contract, tool registry, and event schema your own. Everything else should be an adapter to an existing standard.

That gives you a minimal Flue-like system initially, but one that can grow into a general-purpose agent runtime without redesigning its foundation.
Flue and ag ui

Flue and AG-UI fit together very well. They solve different parts of an agent system:

    Flue is the agent harness: model execution, tools, sandbox, sessions, and lifecycle.

    AG-UI (Agent User Interaction Protocol) is the communication protocol between an agent backend and a frontend, especially for streaming events, tool activity, and shared state.

For your Rust reimplementation of Flue, I would keep the Flue-style API and agent runtime, then add AG-UI as a frontend transport adapter.
1. Architecture

Frontend
React · Web app · Agent dashboard · CopilotKit

AG-UI Protocol
Messages · streaming · tool events · state updates
Your Rust Flue-like Runtime

Agent sessions

Pi-style loop

Rig.rs models

Tool registry

Tokio scheduler

Event stream

MCP
External tools

Sandbox
Shell, files, code

AG-UI should not replace your internal runtime event system. Your Rust core should emit its own typed events, and an AG-UI adapter should translate them into AG-UI events.
2. Important finding: Flue integration is already being planned

I found an open issue in the official AG-UI repository:
Plan the Flue framework integration — Issue #2793

Opened September 18, 2026, it proposes mapping Flue's lifecycle, messages, tool calls, and state to AG-UI. The issue describes integration planning rather than a completed implementation.
GitHub

There is also an issue for shared state, interrupts, and reasoning events.
ag-ui-protocol/ag-ui
3. How I would map Flue to AG-UI

Your harness event


AG-UI event

Operation begins


RUN_STARTED

Model starts answering


TEXT_MESSAGE_START

Model streams tokens


TEXT_MESSAGE_CONTENT

Model finishes message


TEXT_MESSAGE_END

Model requests tool


TOOL_CALL_START / TOOL_CALL_ARGS

Tool call arguments complete


TOOL_CALL_END

Tool returns


TOOL_CALL_RESULT

UI-visible state changes


STATE_DELTA

Operation completes


RUN_FINISHED

Operation fails


RUN_ERROR

These event families are part of the AG-UI specification.
GitHub

One important distinction: a Flue session can contain multiple operations, while an AG-UI run represents one execution. Don't equate a persistent session with a single run.
4. Rust implementation

There is already a Rust crate with AG-UI event types:
ag-ui-protocol on docs.rs

It provides Rust representations of lifecycle, tool, text, and state events. Check its version and compatibility with the AG-UI 1.0 specification before adopting it.
Rust

I would build a small adapter:

pub struct AgUiAdapter {
// Converts internal harness events
// into AG-UI protocol events.
}

impl AgUiAdapter {
pub fn translate(
&self,
event: HarnessEvent,
) -> Vec<AgUiEvent> {
todo!()
}
}

Expose it through an Axum HTTP endpoint with Server-Sent Events (SSE). Keep cancellation and user approvals as separate inbound operations.
5. Should AG-UI be part of your core?

No — but design your core to support it from day one.

Your core needs stable run IDs, session IDs, tool-call IDs, ordered event streams, cancellation, and resumable state. AG-UI then becomes one of several adapters.

That also means you can support:

    AG-UI for React and web applications.

    ACP for IDE integrations.

    MCP for external tools.

    Your native TypeScript SDK for Flue-style agent definitions.

For your project, I'd make AG-UI the first external event protocol, after the Rust execution loop works. It gives you a standardized way to build a real-time frontend without tying your harness to a particular UI framework.
No does it support now flue

No. As of October 25, 2026, AG-UI does not officially support Flue yet.

I checked the AG-UI GitHub repository:

    Flue integration: Planned, not implemented.

    GitHub issue: #2793 — Flue framework integration

    Status: Open, 0 of 5 tasks completed.

    Implementation: No linked pull requests or implementation branches. 
    GitHub

Flue is also absent from AG-UI's official list of supported framework integrations.
GitHub

However, you can integrate Flue with AG-UI yourself by writing an adapter that converts Flue's execution events into AG-UI events.

For your Rust-based Flue recreation, this means AG-UI compatibility would be something you implement, not something you get automatically from Flue.


