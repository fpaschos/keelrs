# Archon's agent runtime: defining, running and messaging an agent

How Archon declares an agent, runs it, sends it messages, names its conversations, and what it logs and resumes;
compared with Flue and keelrs. This note covers the runtime under Archon's workflows; the workflows themselves are in
`research/archon-workflows` (`docs/research/archon-workflows.md`). Sources: Archon at `755cfad79` (2026-11-03,
`../archon`), Flue facts reused from `research/flue-render-and-messaging` (Flue `v2.3.0`, `deee93b4`), keelrs `main`
at `29b20b5`. Each Archon claim cites `path:line` read at that commit; paths under `packages/docs-web/` are Archon's
own docs ("docs" below, relative to `packages/docs-web/src/content/docs/`). Inferences are marked.

## 0. The shape: Archon delegates the model loop

**Archon does not run its own model loop.** Every agent call goes through one provider interface,
`IAgentProvider.sendQuery(prompt, cwd, resumeSessionId?, options?): AsyncGenerator<MessageChunk>`
(`packages/provider-contract/src/agent-provider.ts:290`, `:319-324`). Providers wrap someone else's agent:

- **Claude**: the Claude Agent SDK's `query()` (`packages/providers/src/claude/provider.ts:33-40`, `:727`), i.e.
  Claude Code. A resume sets the SDK's `options.resume` (`provider.ts:546-551`).
- **Codex**: one Codex turn per `sendQuery`, over the `codex app-server` JSON-RPC protocol on its own process
  (`packages/providers/src/codex/provider.ts:1-4`, `:400-410`); the thread id is Archon's session id
  (`codex/provider.ts:181`).
- Community providers: Pi, OpenCode, Copilot (`packages/providers/src/community/`).

So "per turn" inside one `sendQuery` is whatever Claude Code or Codex does. Archon's own loops sit **outside** that
call: a node's retry, the structured-output reask (section 6), and `loop:` nodes, each iteration one more
`sendQuery` (`packages/workflows/src/dag/nodes/loop/iteration-stream.ts:453`).

There are two callers of `sendQuery`, and so two kinds of "agent":

- **The chat orchestrator**: one agent per conversation, answering platform messages (web, Slack, GitHub comments)
  in `handleMessage` (`packages/core/src/orchestrator/orchestrator-agent.ts:79-84`), calling `sendQuery` in
  `packages/core/src/orchestrator/chat-turn.ts:114-117`.
- **A workflow AI node** (`prompt:` or `command:`), calling `sendQuery` in
  `packages/workflows/src/dag/nodes/agent/agent-stream.ts:133-134`.

## 1. Agent definition and render rhythm

**Declared as data, not code.** A workflow agent is a YAML node: `command: { command, with }` or `prompt: { prompt }`
(`packages/workflows/src/schemas/dag-node.ts:165-166`) plus the shared options `model`, `provider`, `context`,
`output_format`, `allowed_tools`, `denied_tools`, `mcp`, `skills`, `plugins`, inline sub-`agents`, `effort`,
`systemPrompt`, `maxBudgetUsd`, `fallbackModel` (`packages/workflows/src/schemas/dag-node-base.ts:61-124`). A
`command:` loads a Markdown prompt template from `.archon/commands` (`dag/nodes/agent/agent.ts:65-71`). The provider
receives the YAML node raw as `NodeConfig` and "translate[s] fields they understand; unknown fields are ignored"
(`agent-provider.ts:206-250`).

**Evaluated once per node attempt.** `loadAgentPrompt` substitutes variables and upstream outputs into the template
once (`dag/nodes/agent/agent-prompt.ts:23-86`), then that prompt and the options go to one `sendQuery`. The model,
tools and system prompt cannot change between the model calls inside it; Claude Code or Codex owns those. Between
calls Archon builds a new request: the next node, retry, reask, or loop iteration.

**The chat agent is rebuilt per message.** Each `handleMessage` re-discovers workflows (`orchestrator-agent.ts:191`),
rebuilds the prompt (`:210`), and rebuilds the system-prompt append from the current projects and workflows
(`packages/core/src/orchestrator/chat-request.ts:375-388`, `prompt-builder.ts:580-596`). Native tools are attached per
message (`orchestrator-agent.ts:671-731`).

## 2. Messaging an agent

**Provider level: a stream.** `sendQuery` yields `ProviderChunk`s: message and thought chunks, tool calls and
updates, warnings, MCP status, compaction, subtasks, hooks, heartbeats (`packages/provider-contract/src/events.ts:205-218`),
then one `result` carrying `sessionId`, `text`, `tokens`, `structuredOutput`, `failure`, `resumed`, `sessionNotFound`
(`packages/provider-contract/src/result.ts:56-104`), then `settled` (`events.ts:220-226`). Request options include
`abortSignal`, `systemPrompt`, `appendSystemPrompt`, `outputFormat`, `forkSession`, `nativeTools`
(`agent-provider.ts:101-139`).

**App level: fire-and-forget, then read the stream or history.** There is no `ask`.

| Call | Signature / shape | Returns |
|---|---|---|
| `handleMessage` | `(platform, conversationId: string, message: string, context: HandleMessageContext): Promise<void>` (`orchestrator-agent.ts:79-84`) | nothing; the reply goes out through `platform.sendMessage` (stream or batch mode, `:396-404`) |
| `ConversationLockManager.acquireLock` | `(conversationId, handler): Promise<{ status: 'started' \| 'queued-conversation' \| 'queued-capacity' }>` (`packages/core/src/utils/conversation-lock.ts:30-32`, `:88-140`) | at once; "Fire-and-forget: don't await here" (`:138`) |
| HTTP send | `POST /api/conversations/{id}/message` with `{ message }` or multipart (`packages/server/src/routes/api.messages.ts:45-57`) | `{ accepted, status }` (`packages/server/src/routes/api-message-dispatch.ts:177-238`); no message or submission id |
| HTTP read | `GET /api/stream/:conversationId` (SSE, `api.messages.ts:187`), `GET /api/conversations/{id}/messages` (`:24-27`) | live chunks; stored history |
| Workflow start | `POST /api/workflows/{name}/run` with `{ conversationId, message, inputs?, ... }` (`packages/server/src/routes/api.workflow-run-launch.ts:28-50`) | HTTP 200 accepted; "Execution-time refusals arrive on conversation SSE" (`:47`) |
| Run read | `GET /api/workflows/runs/{runId}`, `/provider-events` (`packages/server/src/routes/api.workflow-runs.ts:72`, `:88`) | the run, its events |

Inside a workflow, nodes do not message each other; a later node reads an earlier one's output through
`$node.output` substitution (`packages/workflows/src/workflow-variables.ts:50-55`).

## 3. Instance identity, create vs continue

- **Chat**: a conversation is keyed by `(platform_type, platform_conversation_id)` (`migrations/000_combined.sql:87-103`),
  got-or-created on every message (`orchestrator-agent.ts:117-123`). For GitHub the platform id is
  `owner/repo#number` (`packages/adapters/src/forge/github/comment-delivery.ts:27-29`), so one issue is one
  conversation. Under it, one active `remote_agent_sessions` row holds the provider's `assistant_session_id`
  (`000_combined.sql:108-121`); the first message creates it (`orchestrator-agent.ts:222-227`), and each `result`
  stores the new provider id (`chat-turn.ts:34-39`). A new session is started only by a named transition:
  `first-message`, `plan-to-execute`, `isolation-changed`, `project-changed`, `reset-requested`, `worktree-removed`,
  `conversation-closed`, `cwd-changed` (`packages/core/src/schemas/session.ts:5-14`); the old row links to the new
  by `parent_session_id` (`packages/core/src/db/sessions.ts:94-103`).
- **Workflow nodes**: identity is the run id (`remote_agent_workflow_runs.id`, `000_combined.sql:161-185`) plus the
  node id. A node's `context` chooses continue or create: `fresh`, `shared` (continue the previous sequential node's
  session), or `{ resume: node }` (fork that node's session) (`dag-node-base.ts:40-44`). The default is fresh in a
  parallel layer and inherited in a sequential one; a provider change also forces fresh
  (`dag/nodes/agent/dispatch.ts:245-284`). Named resume fails rather than falling back: missing handle, other provider,
  or a provider without immutable forks throws (`dispatch.ts:288-319`). Across runs, `persist_session: true` keys a
  node's session by `(workflow_name, node_id, scope_key, provider)` (`000_combined.sql:225-235`, upsert at
  `dispatch.ts:199-213`).
- **Guards against continuing the wrong thing**: a stale resume is reported as `resumed: false` or
  `sessionNotFound` (`result.ts:86-104`); chat clears the stored id, retries once fresh and tells the user
  (`chat-turn.ts:108-130`); a node keeps a cold-fallback result and warns (`dispatch.ts:149-190`).
- **Idempotency for retried triggers**: forge webhooks are deduplicated in memory by comment id plus `updated_at`,
  falling back to the delivery GUID (`packages/adapters/src/forge/github/webhook-intake.ts:102-118`), TTL 10 minutes
  (`packages/core/src/utils/delivery-dedup.ts:19-22`). Trigger starts are durable: `remote_agent_start_receipts` is
  unique on `(source_instance_id, delivery_id)` (`000_combined.sql:581-593`); replaying a delivery reuses its receipt,
  and the same id with different content is HTTP 409 (docs `guides/workflow-triggers.md:309`).

There is no create-only or continue-only call: chat always get-or-creates, and a workflow start makes a new run
unless the caller resumes one by id (section 7).

## 4. Typed messages

**Into the agent, everything is text.** `sendQuery` takes `prompt: string` (`agent-provider.ts:319-321`); Claude gets
one `{ role: 'user', content: prompt }` message (`claude/provider.ts:583-588`).

- **Chat**: the adapter flattens a platform event to strings. A GitHub comment becomes `message` (issue or PR title,
  author, labels, body, then the comment) and `issueContext` (a short reference)
  (`webhook-intake.ts:120-202`); the last 20 comments become `threadContext` (`:207-236`).
- **Workflows**: typed values reach a prompt by text substitution: `$ARGUMENTS`/`$USER_MESSAGE`, `$CONTEXT`/
  `$ISSUE_CONTEXT`, `$INPUTS.<name>`, `$LOOP_PREV_OUTPUT`, `$REJECTION_REASON`, `$node.output.field`
  (`workflow-variables.ts:61-94`). Issue context is appended when the template did not place it
  (`workflow-variables.ts:257-305`).
- **Trigger events are typed before they become inputs.** GitHub event kinds and fields (`label.actor.login`,
  `comment.author.id`, ...) and Standard Webhooks JSON Pointers map into the workflow's declared `inputs:`, keeping
  their JSON types; an undeclared or missing input rejects the binding (docs `guides/workflow-triggers.md:305`,
  `:358`, `:503`). They still reach the model as substituted text.

## 5. Joining or steering a running agent

**No message reaches a running turn.** Claude gets exactly one user message per `sendQuery`; the input stream then
only waits to close (`claude/provider.ts:570-599`).

- **A second chat message for a busy conversation is queued**, FIFO per conversation, and runs after the current
  one finishes (`conversation-lock.ts:92-96`, `:125-128`, `:163-176`). The queue is an in-memory `Map`
  (`conversation-lock.ts:63-80`), so a restart loses queued messages (inference from the code). The web UI gets a
  `lock` SSE event while queued (`api-message-dispatch.ts:225-235`).
- **Workflow runs are steered between nodes, not inside one**: `cancel`, `resume`, `signal`, `abandon`, `approve`,
  `reject`, `respond`, `answer` (`packages/server/src/routes/api.run-control.ts:42-130`,
  `packages/server/src/routes/api.run-gates.ts:58-124`). Waits and approval gates are the durable way to put a
  person's input into a run (docs `reference/workflow-language.md:685`).
- **Abort**: `abortSignal` on the request (`agent-provider.ts:103`); the Claude provider aborts its controller and
  closes the input (`claude/provider.ts:691-695`, `:717-719`). An operator can also ask to end a stalled tool call's
  turn (`packages/workflows/src/end-turn-request.ts:14-30`).

## 6. Structured output

- A node declares `output_format` (JSON Schema) (`dag-node-base.ts:73`). It goes to the provider as
  `outputFormat: { type: 'json_schema', schema }` (`agent-provider.ts:112`): Claude passes it to the SDK and reads
  `structured_output` off the result (`claude/result.ts:211-212`); Codex sends it as the turn's `outputSchema`,
  normalized to OpenAI strict mode (`codex/turn-params.ts:239-267`, docs `reference/workflow-language.md:581-595`).
- Each provider declares `structuredOutput: 'enforced' | 'best-effort' | false`
  (`packages/provider-contract/src/capabilities.ts:60`). Archon validates the result against the schema for **every**
  provider; a best-effort provider is re-asked with the errors appended, up to 3 times
  (`dag/nodes/agent/agent-reask.ts:33-80`, `:100-107`; `dag/ai-turn.ts:34`). A missing or invalid result fails the node.
- The validated object becomes the node's output, read downstream as `$node.output.field`, with strict field access
  (docs `reference/workflow-language.md:618`).
- The chat path sets no `outputFormat` (`chat-request.ts:381-388`); chat replies are text.

## 7. What is logged, and resume

**Persisted** (`migrations/000_combined.sql`, documented in docs `reference/database.md:199-330`):

- Chat: `remote_agent_conversations`, `remote_agent_sessions` (provider session id, transition reasons),
  `remote_agent_messages` (role, content, metadata) (`000_combined.sql:87-121`, `:244-251`).
- Runs: `remote_agent_workflow_runs` (status, `user_message`, metadata) (`:161-185`) and
  `remote_agent_workflow_events` (`:190-199`), with a global commit-ordered `event_order` (`:488-546`) and an archive
  kept on delete. Event types are listed in `packages/workflows/src/store/event-types.ts:14-140`: node lifecycle
  (`node_started`, `node_completed` with output and spend, ...), waits and approvals, resumes, and since #3569
  `provider_event`, which wraps each provider chunk unchanged with `attemptId`, `seq`, `observedAt`
  (`packages/workflows/src/schemas/provider-event.ts:9-21`).
- Provider session handles: per run and node (`remote_agent_workflow_run_node_sessions`) and per scope across runs
  (`remote_agent_workflow_node_sessions`) (`000_combined.sql:208-235`).
- A JSONL transcript per run, `<logDir>/<runId>.jsonl` (`packages/workflows/src/logger.ts:127-134`), whose row types
  are at `logger.ts:33-53`.
- Trigger receipts, bindings and durable start requests (`000_combined.sql:581-643`).

**What the model saw is not in Archon's store.** No event or JSONL row type carries the resolved prompt or system
prompt (`logger.ts:33-53`, `event-types.ts:14-140`); the conversation itself lives in the provider's own session
(Claude Code's session transcript, Codex's rollout), referenced by id. Archon reads Claude's native transcript only
to subtract spend baselines (docs `reference/node-execution.md:38`). The node record keeps the binding: provider,
requested and resolved model, tier, effort, a session preview, and `sessionOrigin: fresh | resumed |
resume-failed-cold` (docs `reference/node-execution.md:18`, `:245-246`).

**Resume is per run, at node granularity, and explicit.** `archon workflow resume <run-id>` or `run --resume`
(docs `reference/workflow-language.md:956-964`): it loads `node_completed` events and the session handles, skips
completed nodes, and re-runs failed and unrun ones (`:966-970`). A node interrupted mid-turn is re-run from its
prompt; `always_run` opts a node out of the cache (`dag-node-base.ts:120-124`). A server crash does not fail
`running` rows; an operator resumes them after checking the owner is gone (`workflow-language.md:972`). Waits survive
restarts and resume on their deadline or answer (`workflow-language.md:685`, `:770`). Chat has no resume beyond
the stored provider session id: the next message continues it.

## Comparison

| | Archon (`755cfad79`) | Flue (`v2.3.0`) | keelrs (`29b20b5`) |
|---|---|---|---|
| 1. Definition, render rhythm | YAML node (`prompt`/`command` + options) or the chat orchestrator; built once per `sendQuery` (node attempt, reask, loop iteration; chat: per message). The model loop is Claude Code (Agent SDK `query`) or Codex app-server, not Archon (`agent-provider.ts:319`, `claude/provider.ts:727`, `codex/provider.ts:1-4`) | TS agent function rendered at harness init per submission, then before each later model call; model, MCP, sandbox fixed per submission (`client.ts:331`, `session.ts:939`) | Rust agent function rendered before every model call (`rig/mod.rs:107-111`); model connected once per dispatch (`rig/mod.rs:94`, `:128`); own loop on rig |
| 2. Messaging | fire-and-forget: `handleMessage(...) -> Promise<void>`, HTTP `POST .../message -> { accepted, status }`; read via SSE or message history; provider level is an `AsyncGenerator` stream. No `ask` | `dispatch -> DispatchReceipt`, then `read(receipt)`; SDK `send`/`read`/`wait`/`observe`. No `ask` | `dispatch -> Receipt`, `read(&Receipt)`, `ask`, `ask_structured::<T>` (`instance.rs:69-104`) |
| 3. Identity, create vs continue | chat: `(platform, platform_conversation_id)`, got-or-created; active session row holds the provider session id, replaced by named transitions. Workflow: run id + node id; `context: fresh \| shared \| {resume}`; `persist_session` keyed by workflow/node/scope/provider. Stale resume surfaced. Trigger receipts unique on `(source, delivery_id)`; webhook dedup in memory | instance id; `uid` incarnation; `initialData` only at birth; `idempotencyKey` on dispatch | `instance_with(&agent, id, data)`: same id continues, data dropped on an existing id with no error (`runtime.rs:56-73`); no idempotency key |
| 4. Typed messages | text into the model; platform events flattened to `message` + `issueContext` strings; trigger event fields map into typed declared `inputs:`, then substituted as text | `message` with kinds (`user`, `signal`); `useDelivery` reads it | `String` |
| 5. Joining, steering | none inside a turn (one user message per `sendQuery`); a busy conversation queues the next message FIFO in memory; runs take cancel/resume/signal/approve/answer between nodes; abort via signal | a dispatch joins the live response at a turn boundary; `abort` | none: the inbox runs dispatches one at a time; no abort |
| 6. Structured output | node `output_format` JSON Schema → provider `outputFormat`; validated for every provider, best-effort providers reasked up to 3 times; output read as `$node.output.field`. Chat: none | in-run `prompt/skill/task(..., { result })`; app reply is text + named data parts | `ask_structured::<T>` with a finish tool and reasks (`instance.rs:83`) |
| 7. Logged, resume | SQL: conversations, sessions, messages, runs, ordered run events (node lifecycle, raw `provider_event` envelopes), session handles, trigger receipts; JSONL per run. No prompt text; the transcript stays in the provider's session store. Resume: explicit, per run, skip completed nodes, re-run the rest; chat continues the provider session | durable conversation records; no per-render record, `resource_snapshot` on change; resume re-renders | nothing stored yet (`event.rs`: `STORE:`) |

## For keelrs (inference)

These are conclusions drawn from the facts above, aimed at the events map #43 and its tickets #46 (envelope), #47
(instance events) and #49 (store and resume). They are not facts about either codebase.

- **Avoid: leaving the model-visible transcript outside the store.** Archon can, because Claude Code and Codex
  persist their own sessions and Archon keeps only the id. keelrs runs its own loop on rig, so nobody else holds the
  transcript: #47 has to record every message and tool result itself, and the render's declarations if a resumed
  run must rebuild the request (as the Flue note already argued).
- **Take: a provider-event envelope that wraps the raw event unchanged.** `provider_event` adds only what the engine
  knows: `attemptId`, a contiguous `seq` per attempt, `observedAt`, while the row's own frame names the run and node
  (`provider-event.ts:9-21`). For #46: an envelope of instance id, submission id, a per-instance contiguous `seq`, a
  timestamp, and the payload.
- **Take: one global commit order for a feed.** Archon assigns `event_order` at commit under a lock and keeps
  readers on opaque cursors (`000_combined.sql:488-546`, docs `reference/database.md:391-395`). If #49 serves a
  cross-instance feed, a per-store sequence assigned at commit does the same in SQL via sqlx.
- **Take: record what continuing actually did.** `resumed: true | false`, `sessionNotFound`, and
  `sessionOrigin: fresh | resumed | resume-failed-cold` make a silent cold restart visible. For keelrs this maps onto
  `instance_with`: an event, or a return value, that says "created" versus "continued" would also fix the silently
  dropped `data` on an existing id.
- **Take: durable receipts for retried triggers.** Archon's in-memory webhook dedup loses its memory on restart;
  its trigger receipts, unique on `(source, delivery_id)` with a content digest that turns a reused id with new
  content into a conflict, do not. A keelrs `dispatch` idempotency key stored with the submission would give the
  same guarantee (Flue has `idempotencyKey` too).
- **Avoid: an in-memory busy queue.** Archon's per-conversation queue is a `Map`, so queued messages die with the
  process. keelrs's inbox is the same today; once #49 lands, an accepted dispatch should be an event before the
  receipt returns, so a restart can still run it.
- **Resume granularity.** Archon re-runs an interrupted node from its prompt and trusts the provider session for
  context; it never resumes mid-turn. keelrs could take the same unit for #49: a dispatch interrupted mid-run is
  re-run from its last committed turn (its events), not replayed tool by tool. Tool side effects would then repeat,
  as Archon's re-run nodes do.
- **Structured output is validated by the runtime, whatever the model promises.** Archon validates even
  "enforced" providers and reasks only best-effort ones (`agent-reask.ts:100-107`). keelrs already validates and
  reasks; an event per reask (Archon emits one, `agent-reask.ts:77`) would make them visible in #47.
