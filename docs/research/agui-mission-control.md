# A mission control for keelrs: generative UI, AG-UI and a crew

This file records and checks a design conversation about a web "mission control" for keelrs agents. Its inputs are a colleague's Emacs setup, the AG-UI protocol, A2UI, CopilotKit and Flue. It relates them to map #43 and tickets #47, #92, #93 and #36. Sections 1 to 4 are facts with citations. Sections 5 and 6 are inference for keelrs. Section 7 lists each claim that the sources contradict or do not confirm. This file makes no decisions.

## Sources

- **The conversation**: a Claude Code session of 2026-11-20 (session name `keelrs-event-feed-options`). It is not in the repo.
- **The older design record**: `docs/flue-rust-harness-conversation.md`, sections 6 to 9 (Q6 to Q9) and "Facts to re-verify" (lines 425-493, 566-575).
- **A colleague's Emacs setup**: local `../doom` at `116f8a2` (2026-11-18). `DOOM/` = `../doom/`.
- **Flue**: local `../flue`, runtime `2.3.0` (`packages/runtime/package.json`). `FLUE/` = `../flue/packages/`.
- **AG-UI**: `github.com/ag-ui-protocol/ag-ui` at `ab01f98` (main, 2026-11-19). `AGUI/` = `https://github.com/ag-ui-protocol/ag-ui/blob/ab01f98214529e64d7857b16617c094afc360ad0/`. npm `@ag-ui/core` (https://registry.npmjs.org/@ag-ui/core).
- **A2UI**: `https://github.com/a2ui-project/a2ui` at `496df0e7` (2026-11-20); `https://a2ui.org/specification/versions.json`.
- **CopilotKit**: `github.com/CopilotKit/CopilotKit` at `c1f317e8`. `CK/` = `https://github.com/CopilotKit/CopilotKit/blob/c1f317e839996dc845b930567210c8d976d6c5b1/`.
- **crates.io API** (`https://crates.io/api/v1/crates/<name>`), read on 2026-11-20.
- **keelrs**: `main` at `6180792`. Tickets read with `gh issue view`.

## Summary

- **The colleague's generative UI is a fixed catalog of JSON elements.** The agent shows, updates and appends elements by `id` through a shell command or MCP tools. Emacs draws them on a canvas per session. Interactive elements send the user's answer back as the next prompt (section 1).
- **The colleague's crew is many Claude Code sessions over ACP**, one per git worktree, with role files and Beads issues. The code supports agent-to-agent messages, but `config.el` turns them off. With them off, a role's `hands-to` is ignored too (section 1b).
- **AG-UI is at spec 1.0** (frozen 2026-09-17). It has 31 event types in 8 groups, reasoning and activity events included. A client `POST`s a `RunAgentInput` and reads SSE from the response (section 2).
- **Flue uses the same architecture with its own wire format.** It projects private records into 12 chunk types and resumes from an offset. Flue has no AG-UI code (section 3).
- **A2UI is at 1.0** ("current"). It travels inside AG-UI as `ACTIVITY_*` events, through `@ag-ui/a2ui-middleware`. CopilotKit has an A2UI renderer. Its frontend-only `selfManagedAgents` is an Enterprise feature. Rust has only community crates (section 4).
- **keelrs must own two things**: a public event feed (#92) and public, stable event types (#47). The axum server, the AG-UI mapper, the `show` tool and the page can live in the application (section 5).
- **A crew can start without agent-to-agent**, as the colleague's setup runs today. It needs #92 with a runtime-wide bus (#92 Q2). The rest is application code (section 6).

## 1. How a colleague's Emacs setup does generative UI and a crew

The setup is a Doom Emacs config. Claude Code is the harness. Emacs embeds it through the `agent-shell` package over ACP: "Claude Code inside Emacs (agent-shell, via ACP)" (`DOOM/config/agent-shell.el:2`; pin at `DOOM/packages.el:58-65`).

### 1a. Generative UI ("Agent UI")

- **A catalog of element types in Emacs Lisp.** One file per type in `DOOM/config/agent-ui/elements/` (30 files: `progress`, `table`, `chart`, `steps`, `diff`, `graph`, `mermaid`, `panel`, `choice`, `form` and others). The macro `agent-ui-define` defines a type (`DOOM/config/agent-ui/core.el:152`). A type has a `:text` renderer and an optional `:gui` renderer (`core.el:163-166`).
- **Elements are JSON, changed in place by `id`.** The command `DOOM/cmd/agent-ui` has `show`, `update ID`, `append ID`, `remove`, `clear`, `list`, `get`, `types`, `batch`, `render`, `snapshot`, `define` and others (`cmd/agent-ui:6-49`). "its id there already: replaced in place" (`cmd/agent-ui:7`).
- **Transport.** The script makes a temp folder (`cmd/agent-ui:158`) and runs `emacsclient --eval '(agent-ui-request "DIR")'` (`cmd/agent-ui:168-169`). The reply is JSON on stdout: `{"ok": true, ...}` or `{"ok": false, "error": ...}` (`cmd/agent-ui:55-56`).
- **The same operations as MCP tools.** `DOOM/cmd/agent-ui-mcp.el` runs as a batch Emacs. It is added to `agent-shell-mcp-servers` (`DOOM/config/agent-ui.el:43-47, 213-226`).
- **One canvas per session.** Advice on `agent-shell--make-acp-client` adds `AGENT_UI_SESSION=<session>` to the agent's environment (`DOOM/config/agent-ui/agent.el:42-51`).
- **Redraws are throttled.** An update marks the element dirty. A timer redraws dirty elements after `agent-ui-redraw-delay`, 0.1 s (`DOOM/config/agent-ui/canvas.el:8-11, 18`; `DOOM/docs/agent-ui.md:136-137`). Table rows and chart values are capped, "as it runs in Emacs's only thread" (`docs/agent-ui.md:195-196`).
- **The model learns the catalog** from a paragraph in the system prompt (`DOOM/config/agent-ui/agent.el:180-221`) and from the skill `DOOM/skills/agent-ui/SKILL.md`. `agent-ui types` returns the schemas (`cmd/agent-ui:22`).
- **Self-correction.** An unknown property fails with "unknown property %s; its properties: %s" (`DOOM/config/agent-ui/core.el:298`).
- **Answers come back as the next prompt.** A `choice` puts "Answer to choice ID (QUESTION): ..." into the session's prompt (`DOOM/config/agent-ui/elements/choice.el:7, 84`; the format in `DOOM/config/agent-ui/ask.el:176-178`).
- **Sources.** An element can follow a command, a file or a poll: `{"file": "build.log", ...}`, `{"poll": "df -h /", "every": 10}` (`DOOM/config/agent-ui/source.el:2-12`).
- **New types from the agent.** `agent-ui define` stages a type; the user keeps or discards it (`cmd/agent-ui:32-34`; `DOOM/config/agent-ui/staging.el:2`).

### 1b. The crew ("agent-workspace")

- **One agent is one agent-shell session** in its own git worktree. The folder is `.agent-shell/worktrees/<role>-<name>` (`DOOM/config/agent-workspace/worktree.el:24, 46`). Its branch is `agent/<name>` at rest (`DOOM/config/agent-workspace/core.el:152`).
- **Roles are Markdown files with front matter**: `tools`, `deny`, `labels`, `hands-to`, `max` and display fields (`DOOM/config/agent-workspace/roles/builder.md:1-12`). 12 starter roles are in `DOOM/config/agent-workspace/roles/`.
- **Beads issues carry the work.** A builder claims its issue with `bd update ID --claim` (`roles/builder.md:15`). A looping agent takes the next ready issue with `bd ready --claim` (`DOOM/config/agent-workspace/loops.el:16`; `DOOM/docs/agent-workspace.md:285`).
- **The app has three parts**: the issues on the left, the stage in the middle, the room on the right (`DOOM/docs/agent-workspace.md:4, 25`). In the room, `#ada ...` goes to one agent, `#builders ...` to the first free builder, `#all ...` to everyone (`DOOM/config/agent-workspace/room.el:19-20`).
- **A minimap** shows each agent's last 15 minutes (`DOOM/config/agent-workspace/activity.el:26`) and the files that two agents changed (`DOOM/config/agent-workspace/minimap.el:125-129, 284-293`).
- **State** is in `.agent-shell/agent-workspace/crew.json` (`DOOM/config/agent-workspace/saved.el:56`) and `room.jsonl` (`DOOM/config/agent-workspace/room.el:190`).
- **Agent-to-agent exists in the code but is off.**
  - Direct messages: an agent runs `cmd/agent-workspace send --to ken TEXT`. A role's `talks-to` names who it may message without the user's OK. A message goes through an inbox and "is delivered when its agent is free". Its answer goes back, with "no answer to an answer" (`DOOM/config/agent-workspace/messages.el:1-19`).
  - `agent-workspace-agents-talk` and `agent-workspace-outside-events` default to `t` (`DOOM/config/agent-workspace.el:13, 23`). `DOOM/config.el:135-136` sets both to `nil`.
  - With `agents-talk` off, "a role's hands-to [is] ignored (it's told to hand nothing on)" (`DOOM/config/agent-workspace.el:14-19`). So today the user hands each issue on.
- **Offline demos**: the examples `agent-ui` and `agent-workspace` (`DOOM/docs/examples.md:17, 20`), run with `M-x +my/example` (`DOOM/config/examples.el:48-53`).

## 2. AG-UI

### 2a. Version

- The spec is **1.0**. `PROTOCOL_VERSION = "1.0"` (`AGUI/sdks/typescript/packages/core/src/generated/version.ts`). The schema id is `https://ag-ui.com/spec/1.0/schema.json`. Commit `fdbca490` froze it on 2026-09-17 ("feat(spec): freeze the draft as 1.0").
- npm `@ag-ui/core`: latest `1.0.5` (2026-11-17); `1.0.0` on 2026-09-17. GitHub releases are date tags (`release/2026-11-17`).
- The TypeScript types are generated from the schema. The old `events.ts` is gone. The enum is at `AGUI/sdks/typescript/packages/core/src/generated/types.ts:9-41`.

### 2b. Event groups and types

From `AGUI/docs/concepts/events.mdx:12-26` (overview) and `:56-809` (sections). The enum has 31 members.

| Group | Events |
|---|---|
| Lifecycle | `RUN_STARTED`, `RUN_FINISHED`, `RUN_ERROR`, `STEP_STARTED`, `STEP_FINISHED` |
| Text message | `TEXT_MESSAGE_START`, `TEXT_MESSAGE_CONTENT`, `TEXT_MESSAGE_END`, `TEXT_MESSAGE_CHUNK` |
| Tool call | `TOOL_CALL_START`, `TOOL_CALL_ARGS`, `TOOL_CALL_END`, `TOOL_CALL_RESULT`, `TOOL_CALL_CHUNK` |
| State | `STATE_SNAPSHOT`, `STATE_DELTA`, `MESSAGES_SNAPSHOT` |
| Activity | `ACTIVITY_SNAPSHOT`, `ACTIVITY_DELTA` |
| Special | `RAW`, `CUSTOM` |
| Reasoning | `REASONING_START`, `REASONING_MESSAGE_START`, `REASONING_MESSAGE_CONTENT`, `REASONING_MESSAGE_END`, `REASONING_MESSAGE_CHUNK`, `REASONING_END`, `REASONING_ENCRYPTED_VALUE` |
| Subagent | `SUBAGENT_STARTED`, `SUBAGENT_FINISHED`, `SUBAGENT_ERROR` |

- **Reasoning and activity events exist.**
- **`THINKING_*` is deprecated.** It is not in the 1.0 enum. `events.mdx:809` says it "will be removed in version 1.0.0". `AGUI/DEPRECATIONS.md:30-36` keeps an inbound shim that maps `THINKING_*` to `REASONING_*` until 2027-09-17.
- "Meta events" and "modified lifecycle events" are drafts, not in the enum (`events.mdx`).

### 2c. Transport and ids

- **`RunAgentInput`** (`AGUI/sdks/typescript/packages/core/src/generated/types.ts:1578-1628`): `threadId`, `runId`, `protocolVersion?`, `parentRunId?`, `state?`, `messages`, `tools`, `context`, `forwardedProps?`, `resume?`. Only `threadId`, `runId` and `messages` are required on the wire.
- **`threadId`** is "The conversation this run belongs to". **`runId`** "Identifies this run" (same file). One thread holds many runs. The older record says the same: "Don't equate session with run" (`docs/flue-rust-harness-conversation.md:446-447`).
- **The client `POST`s and reads SSE from the response.** `HttpAgent.requestInit` sends `method: "POST"`, `Content-Type: application/json`, `Accept: text/event-stream`, with the JSON `RunAgentInput` as body (`AGUI/sdks/typescript/packages/client/src/agent/http.ts:53-64`). The response is parsed as SSE, or as protobuf when its content type is `AGUI_MEDIA_TYPE` (`AGUI/sdks/typescript/packages/client/src/transform/http.ts:66-76`).
- No `GET` / `EventSource` path is in the TypeScript client. Whether the protocol allows one is **not verified**.

## 3. Flue's equivalent

Flue does not use AG-UI. A search of `../flue/packages` for "ag-ui", "agui" and "copilotkit" finds nothing. A search for "a2a" finds nothing either.

- **Private records, projected to wire chunks.** `projectConversationRead(state, read, windowReset)` "Project[s] one durable read window into public conversation chunks, advancing the reduced state batch by batch" (`FLUE/runtime/src/runtime/conversation-observer.ts:37-60`). It reduces each batch of records with `reduceConversationRecords`, then emits chunks.
- **The updates route** (`FLUE/runtime/src/runtime/handle-conversation-routes.ts:181-245`):
  - It reads `offset` and `live` (`long-poll` or `sse`) from the URL (`:191-193`, `liveMode` at `:405-411`).
  - An offset past the head fails with a 416 `StreamOffsetGoneError` (`:199-211`).
  - Each response starts with a `stream-checkpoint` chunk that carries the stream's incarnation (`:212-220`).
  - For SSE it calls `sseResponse` (`:221-230`, defined at `:304`). The SSE loop reads the store from `currentOffset`, projects, writes `event: data`, and moves the offset (`:345-355`). The content type is `text/event-stream` (`:386`). A heartbeat runs every 15 s (`:41`).
- **Resume from an offset.** Responses carry `Stream-Next-Offset` (`handle-conversation-routes.ts:116, 175`). An accepted prompt answers 202 with `Location` and `Stream-Next-Offset` (`FLUE/runtime/src/runtime/handle-agent.ts:65-82`). The SDK keeps that offset as its resume checkpoint (`FLUE/sdk/src/public/stream.ts:65-66`). Each chunk carries a monotonic `position`, and `observe()` drops redelivered chunks (`FLUE/sdk/src/public/conversation-stream.ts:22-36`).
- **The chunk types** (`FLUE/sdk/src/public/conversation-stream.ts:38-149`):

| Chunk | Fields of note |
|---|---|
| `conversation-reset` | `snapshot` |
| `message-appended` | `message` |
| `message-started` | `messageId`, `submissionId?`, `turnId?`, `metadata?` |
| `message-metadata` | `metadata` |
| `data-part` | `name`, `data` |
| `message-delta` | `kind: 'text' \| 'reasoning'`, `delta` |
| `tool-input` | `toolCallId`, `toolName`, `input` |
| `tool-output` | `toolCallId`, `output`, `durationMs?` |
| `tool-output-error` | `toolCallId`, `errorText` |
| `message-completed` | `messageId` |
| `submission-settled` | `submissionId`, `outcome: 'completed' \| 'failed' \| 'aborted'` |
| `stream-checkpoint` | `incarnation`; wire-only, no `position` |

- **Flue ships its own client**: `@flue/sdk` and `@flue/react` with `useFlueAgent()` (`FLUE/react/src/use-agent.ts:64`).
- **Rough mapping to AG-UI** (inference): `message-started` / `message-delta` (text) / `message-completed` → `TEXT_MESSAGE_START` / `CONTENT` / `END`; `message-delta` (reasoning) → `REASONING_MESSAGE_*`; `tool-input` → `TOOL_CALL_START` + `ARGS` + `END`; `tool-output` → `TOOL_CALL_RESULT`; `submission-settled` → `RUN_FINISHED` or `RUN_ERROR`; `conversation-reset` → `MESSAGES_SNAPSHOT`.

## 4. Generative UI

### 4a. The `show` host tool and a skill

This is the pattern of section 1a, moved onto the event path (inference, from the conversation):

- A **skill** lists the element types and says when to use each, as `DOOM/skills/agent-ui/SKILL.md` does.
- A **`show` host tool** takes the element JSON. It can check it against the catalog and return an error, as `DOOM/config/agent-ui/core.el:298` does. It sends nothing itself.
- The tool call is an ordinary event. The UI reads it from the event stream and draws it, keyed by `id`. A page reload replays it.
- The user's answer is a new dispatch, as in `DOOM/config/agent-ui/ask.el:176-178`.

### 4b. A2UI

- **The repo moved** from `github.com/google/A2UI` to `https://github.com/a2ui-project/a2ui`.
- **Versions** (`https://a2ui.org/specification/versions.json`): v1.0 "current", v0.9.1 and v0.9 "previous", v0.8 "deprecated". The spec folders are `v0_8`, `v0_9`, `v0_9_1`, `v1_0`. GitHub releases are SDK tags only (for example `python/a2ui-agent-sdk/v0.12.0`, 2026-11-17).
- **In AG-UI, A2UI travels as activity events.** `@ag-ui/a2ui-middleware` (npm `0.0.16`, 2026-11-12) streams `a2ui-surface` activities whose content holds `a2ui_operations` (`AGUI/middlewares/a2ui-middleware/README.md`, `src/activity-snapshots.ts`). It builds them from a `render_a2ui` tool call, which `injectA2UITool` can add. It forwards the `TOOL_CALL_RESULT` unchanged. It emits `version: "1.0"` when the catalog declares `protocolVersion: "1.0"`, else `"v0.9"`.
- AG-UI lists A2UI as a supported generative UI spec (`AGUI/docs/concepts/generative-ui-specs.mdx:14`).
- **Rust:** no official A2UI crate. Community crates exist: `a2ui` 0.3.1 (Liangdi/a2ui); `a2ui-core`, `a2ui-types`, `a2ui-validation` 0.2.0 (applegrew/a2ui-rs, 2026-11-11); `ag-ui-a2ui` 0.5.0-alpha.3 (KimSoungRyoul/ag-ui-rust). Source: `https://crates.io/api/v1/crates?q=a2ui`.

### 4c. CopilotKit

- **`HttpAgent` in `CopilotRuntime`**: "`agents` takes `AbstractAgent` instances… Pass the `@ag-ui/<framework>` wrapper for it, or a plain `HttpAgent`". Registration: `new CopilotRuntime({ agents: { default: myAgent } })` (`CK/showcase/shell-docs/src/content/docs/(shared)/backend/copilot-runtime.mdx:122-123, 265-266`).
- **Frontend only, with no runtime**: `<CopilotKit selfManagedAgents={{ "support-agent": new HttpAgent({url}) }}>` for production, or `agents__unsafe_dev_only` (React) for local development (`CK/showcase/shell-docs/src/content/docs/(shared)/backend/self-managed-agents.mdx`).
- **`selfManagedAgents` is paid**: "`selfManagedAgents` is part of the CopilotKit Intelligence Enterprise plan. Talk to an engineer about licensing for production use." (same file, lines 65-68).
- **A2UI renderer**: `@copilotkit/a2ui-renderer` `1.90.0` (`CK/packages/a2ui-renderer`), and `CK/packages/angular/a2ui`. Its docs page uses A2UI v0.9 through the `a2ui` prop; v1.0 is under "Components and surfaces" (`CK/docs/generative-ui/a2ui/index.mdx`).

### 4d. Test tools

- **AG-UI Dojo** runs locally from `apps/dojo` with `pnpm i` and `pnpm dev` (`AGUI/apps/dojo/README.md:15-81`). Each integration's URL is an environment variable, for example `SERVER_STARTER_URL`, default `http://localhost:8000` (`AGUI/apps/dojo/src/env.ts:61`).
- A **hosted Dojo** exists at `https://dojo.ag-ui.com` ("CopilotKit Interactive Dojo"). Whether it can point at a custom URL is **not verified**.
- **A2UI Composer**: `https://a2ui.org/composer/` now redirects to `/quickstart/#other-fast-paths`. A Composer page at that URL is **not confirmed**.
- `a2ui-lab.southleft.com` and `hia2ui.pages.dev` were named in the conversation. They are third-party and **not verified** here.

### 4e. Rust crates for AG-UI

| Crate | Latest | Date | Repository | Note |
|---|---|---|---|---|
| `ag-ui-core` | 0.1.0 (only version) | 2025-08-12 | none listed | source in `AGUI/sdks/community/rust/crates/ag-ui-core` |
| `ag-ui-client` | 0.1.0 (only version) | 2025-08-12 | none listed | same folder |
| `ag-ui-protocol` | 0.1.0 | 2026-07-12 | `github.com/kaija/ag-ui-rust` | community |

- The Rust SDK in the AG-UI repo is under `sdks/community/rust`, not with the official SDKs (TypeScript, Python, .NET). Its last commit is `1e10b349` (2026-07-12). No release since 2025-08-12 contains the 1.0 spec (inference from the dates). Treat both crates as stale.
- Google ADK: PyPI `ag_ui_adk` 0.13.0 (2026-11-12, https://pypi.org/pypi/ag_ui_adk/json); npm `@ag-ui/adk` 0.0.3 (2026-10-12).

## 5. The split for keelrs

### 5a. What keelrs has today (`main` at `6180792`)

- `Event` is `pub(crate)` with `Created`, `Dispatched`, `Message`, `ToolResult` and `Settled` (`keelrs/src/event.rs:18-32`).
- `StoredEvent` is public: `id`, `agent`, `instance`, `seq`, `dispatch`, `at`, `kind: String` (`"type"` on the wire) and `event: serde_json::Value` (`keelrs/src/store/mod.rs:109-126`).
- `StoreBackend` is a public trait (`store/mod.rs:162`). The in-memory backend `Memory` is `pub(super)` (`keelrs/src/store/memory.rs:14`). `Store::read` is `pub(crate)` (`store/mod.rs:272`).
- `AgentInstance` has `dispatch`, `ask`, `read` and `abort` (`keelrs/src/agent/instance/mod.rs:119, 125, 154, 165`).
- So an application cannot watch events live today.

### 5b. What goes where (inference)

| Piece | Where | Ticket |
|---|---|---|
| A live event feed: catch up from a `seq`, then live | **keelrs** | #92 Q3 (`subscribe(from: seq)`), Q7 (a plain call returning plain serde events, a `Stream`) |
| A feed across all instances, for mission control | **keelrs** | #92 Q2 (a runtime-wide bus, still open) |
| Text and reasoning deltas, live only | **keelrs** | #92 Q4; #47 (never stored) |
| Public, stable event types that a mapper can read | **keelrs** | #47 (payloads); #92 ("a typed public event" or `StoredEvent`) |
| The AG-UI mapper (keelrs event → AG-UI events) | application first; a keelrs feature or crate only if every application repeats it (AGENTS.md success metrics) | none yet |
| The axum `POST` endpoint with an SSE response | application | none yet |
| The `show` tool and its skill | application first; a built-in tool later, as `activate_skill` is | none yet |
| The page (CopilotKit, `@ag-ui/client`, or plain JavaScript) | application | none |

- **The bus has many consumers.** #93 turns bus events into `tracing` events. #36 turns them into OTel GenAI spans. An AG-UI consumer is one more of the same shape: subscribe, map, write out. #36 says "spans are a view of the events, never a second source". The same rule holds for the AG-UI stream.
- **Plain events stay the source of truth; AG-UI is a projection.** keelrs stores its own events (ADR 0004). The mapper makes AG-UI events on the fly. This is what Flue does with its own chunks (section 3).
- **One AG-UI `POST` dispatches and then streams.** The client sends `RunAgentInput` and reads SSE from the same response (section 2c). So the endpoint calls `dispatch`, then streams that dispatch's events from the bus. A reconnect or a mission-control view that is not a run needs a second path; AG-UI does not define it (inference).
- **Ids** (inference): `threadId` ↔ the agent instance; `runId` ↔ the dispatch id; `seq` ↔ the SSE `Last-Event-ID` that #92 Q3 names.

### 5c. Map order

The order on #43 is: #91 storage → #47 events → #49 restart → #92 bus → #99 write throughput → #93 tracing → #36 OTel. An AG-UI consumer would come after #92.

## 6. Crew stages (inference)

### 6a. Stages

| Stage | What | Needs from keelrs |
|---|---|---|
| 1 | Interactive agent: `POST` AG-UI endpoint, SSE, `show` tool and skill, a page | #92 |
| 2 | Mission control: all instances, their state, raised hands, one feed | #92 Q2 (runtime-wide bus) |
| 3 | Roles: one `Agent` per role, one `AgentInstance` per member, one sandbox per instance (named by a **Sandbox key**, ADR 0006) | nothing new |
| 4 | Tickets: ticket host tools in the application; the user assigns work by `dispatch` | nothing new |
| later | Agents claim tickets themselves; a dispatcher flow (flow tickets #72, #73); `ask_agent`; A2A | new decisions |

- **Prompt while the agent works**: #43 decided "one dispatch at a time" (2026-11-04). A new prompt waits until the current dispatch settles. The colleague's inbox also delivers a message "when its agent is free" (`DOOM/config/agent-workspace/messages.el:10-13`).
- **Approval mid-turn** is not possible today: a turn cannot pause for a human. The agent can ask with `show` and end its turn.
- **Start without agent-to-agent**, as the colleague's setup runs today (`DOOM/config.el:135-136`). The user is the router. Only the user dispatches, so there is no deadlock and no claim race.

### 6b. `ask_agent` and deadlock

Under one dispatch at a time, if agent A's tool waits for agent B's reply while B's tool waits for A, both stop. Options: `ask_agent` returns a receipt and does not wait, or the runtime finds cycles. The colleague's design avoids the cycle in two ways: a message waits in an inbox until the agent is free, and "no answer to an answer" (`DOOM/config/agent-workspace/messages.el:10-15`). This is a design decision for later.

### 6c. ACP vs A2A vs MCP

| Protocol | Connects | Source | In the colleague's setup |
|---|---|---|---|
| ACP (Agent Client Protocol) | an editor ↔ a coding agent | "standardizes communication between code editors … and coding agents" (https://github.com/agentclientprotocol/agent-client-protocol) | **Yes**: agent-shell talks ACP to Claude Code (`DOOM/config/agent-shell.el:2, 19`) |
| A2A (Agent2Agent) | an agent ↔ another agent, across systems | "an open protocol enabling communication and interoperability between opaque agentic applications" (https://github.com/a2aproject/A2A) | **No**: no "a2a" in `DOOM/config`, `DOOM/cmd` or `DOOM/docs` |
| MCP | an agent ↔ tools | modelcontextprotocol.io | **Yes**: Agent UI is an MCP server (`DOOM/config/agent-ui.el:213-226`) |
| MCP Apps | an MCP server returns interactive HTML that a chat host draws | https://modelcontextprotocol.io/extensions/apps/overview | No |

- ACP has Rust crates: `agent-client-protocol` and `agent-client-protocol-schema`.
- **A2A belongs at the edge only**: it exposes keelrs agents to agents outside keelrs, or calls theirs. Inside one keelrs runtime, tickets and dispatches do the job (inference).
- An official A2A Rust SDK exists: `https://github.com/a2aproject/a2a-rs` (created 2026-04-03, targets A2A v1). Its crates.io badge names `a2a-lf` 0.4.2 (2026-10-16).

## 7. Corrections

Each item is a claim from the conversation (C) or from `docs/flue-rust-harness-conversation.md` (D), with what the sources say.

1. **(C) "Agent-to-agent messages are off; the crew agents talk only through Beads issues and the room."** Partly wrong. The code has direct messages with an inbox and `talks-to` (`DOOM/config/agent-workspace/messages.el:1-19`). Both switches default to `t` (`DOOM/config/agent-workspace.el:13, 23`); only `DOOM/config.el:135-136` turns them off.
2. **(C) "A builder finishes and relabels the issue for the next role."** Not true with the current config. With `agents-talk` off, a role's `hands-to` is ignored and the agent "is told to hand nothing on" (`DOOM/config/agent-workspace.el:14-19`).
3. **(C) "The A/B/C question is already settled in #92."** Wrong. #92 is open. Its Q2 to Q7 are recommendations "to grill", not decisions. Q2 (a runtime-wide bus) is an open question.
4. **(C) Map order "#47 → #91 → #49 → #92".** Wrong. #43 puts #91 before #47, and #99 between #92 and #93.
5. **(C) "Newer versions also have activity and reasoning events. Check the current spec."** Confirmed, and the list is longer. AG-UI 1.0 also has `*_CHUNK` events and three `SUBAGENT_*` events. `THINKING_*` is deprecated (section 2b).
6. **(C, D) AG-UI 1.0 / "the spec changes on CopilotKit's schedule".** AG-UI 1.0 exists, frozen on 2026-09-17, with a deprecation policy (`AGUI/DEPRECATIONS.md`). The repo is the `ag-ui-protocol` organization. Who governs the spec is **not verified**.
7. **(C) "Sources disagree on whether A2UI is at 1.0 or v0.9."** Resolved: v1.0 is "current" (`a2ui.org/specification/versions.json`). The repo moved to `a2ui-project/a2ui`. CopilotKit's A2UI docs page still uses v0.9.
8. **(C) "A2UI … designed to travel over A2A."** **Not verified.** In AG-UI it travels as `ACTIVITY_*` events through `@ag-ui/a2ui-middleware` (section 4b).
9. **(C) "AG-UI Dojo … runs locally, not online. I found no hosted page."** Partly wrong. `https://dojo.ag-ui.com` is hosted. A custom URL on the hosted Dojo is **not verified**.
10. **(C) "A2UI Composer at a2ui.org/composer."** Not confirmed. The URL now redirects to the quickstart page.
11. **(C) "The frontend-only path … needs no Node server; enough for a first test" next to "avoid `selfManagedAgents`".** These conflict. The frontend-only production path *is* `selfManagedAgents` (Enterprise). The free frontend-only path is `agents__unsafe_dev_only`, for development only.
12. **(C) "`ag-ui-core` 0.1.0 … may be stale."** Confirmed stale: 0.1.0 is the only release (2025-08-12), before the 1.0 spec. The cited page `docs.copilotkit.ai/ag-ui/sdk/rust/overview` was **not checked**.
13. **(C) "No Rust SDK found" for A2UI.** True for an official SDK. Community crates exist (section 4b).
14. **(C) Flue's chunk list.** It missed `message-appended`. `message-delta` also carries `kind: 'reasoning'`. The route citation `handle-conversation-routes.ts:348` is the `projectConversationRead` call inside `sseResponse` (`:304`), not the route.
15. **(D) Q7: ag-ui issue #2793 "Flue framework integration", open.** Confirmed open. The exact title is "[Feature]: Plan the Flue framework integration", created 2026-09-18 by `contextablemark`. It asks only for a design note (a mapping table, adapter or middleware, layout, open questions). The "0 of 5 tasks" detail was **not checked**.
16. **(D) "The `ag-ui-protocol` crate on docs.rs has Rust event types."** The crate exists (0.1.0, 2026-07-12, `kaija/ag-ui-rust`). It is a community crate, separate from `ag-ui-core`. Its 1.0 support is **not verified**.
17. **(D) "Use the official A2A Rust SDK."** It exists: `a2aproject/a2a-rs`, crates.io `a2a-lf` 0.4.2.
18. **(D) Q9: "no verified native A2A support in Flue".** Consistent: no "a2a" in `../flue/packages`. The Flue README does not mention A2A or AG-UI.
19. **(#43) "Today `keelrs/src/event.rs` has only `Message` and `Settled`."** Stale. `main` has five variants (section 5a).
20. **Not verified at all** (named in the conversation, not checked here): Gemini Enterprise "AG-UI out of scope"; Oracle Agent Spec support; A2UI renderers for Flutter, Lit, Angular and `@a2ui/react`; CopilotKit Vue and Slack renderers; `STATE_DELTA` as JSON Patch; the field names in the example SSE stream; editor ACP support in Neovim, JetBrains and VS Code.
