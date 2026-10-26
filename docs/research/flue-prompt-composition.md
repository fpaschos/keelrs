# How Flue composes the prompt for `useInstruction` and `useSkill`

Research for issue #10 (map: #7). These are ideas for keelrs to borrow, not code to port. No design decision is made here.

**Source.** Flue (`withastro/flue`) at commit `deee93b` (2026-10-12). Paths are relative to `packages/runtime/src/` unless they start with `packages/` or `apps/`. Docs are under `apps/docs/src/content/docs/`.

## Summary

- A render produces two separate things. The first is the **instruction document**: the agent function's returned text, then each `useInstruction` text in call order, joined with blank lines. The second is the **system prompt**, which wraps that document with AGENTS.md, the skill catalog, the subagent roster, the date and workspace facts.
- The skill catalog is one Markdown bullet per skill, `- **name** — description`, under a `## Available Skills` heading. It costs one line per mounted skill.
- `activate_skill { name: string }` returns the full SKILL.md body as the **tool result**, so activating a skill never changes the system prompt. `read_skill_resource { path, offset?, limit? }` returns one packaged file, paged by line.
- Packaged skills are built at compile time: the Vite plugin turns a `SKILL.md` import into an in-memory directory of base64 files with a content-hash id. The model reads those files through the virtual path `/.flue/packaged-skills/<id>/<path>`.
- The skill catalog, subagent roster, AGENTS.md and directory listing are **frozen** on a durable baseline. Mid-conversation changes reach the model as appended `<signal>` user messages, and only compaction refreshes the baseline. The instruction document is the one part that is **live**: it is recomposed on every turn, and a change is flagged with a `System instructions updated.` signal.

## 1. Composition order

### 1a. The instruction document (render)

`composeAgentDocument` puts the returned string first, then the `useInstruction` contributions:

> `const parts = [...(base !== undefined && base.length > 0 ? [base] : []), ...frame.instructions];` … `return parts.join('\n\n');`
> — `hooks/render.ts:222-224`

The doc comment says Flue adds no formatting of its own: "Authors own all formatting — the runtime only joins with blank lines" (`hooks/render.ts:218-219`).

`useInstruction` pushes onto `frame.instructions`. It rejects blank strings and works only while a render is running (`hooks/use-instruction.ts:20-25`). Hooks record on one flat frame in call order, so a `useInstruction` inside a custom hook lands exactly where that hook was called (`hooks/frame.ts:26-28`). `useSkill` contributes nothing to this document. It only pushes onto `frame.skills` and throws on duplicate names (`hooks/use-skill.ts:42-47`).

### 1b. The system prompt (session)

`composeSystemPrompt` in `context.ts:132-202` builds the sections in this fixed order, separated by blank lines:

1. **Instruction document** (from 1a): `if (instructions) pushSection(instructions);` (`context.ts:145`)
2. **AGENTS.md, then CLAUDE.md**, read from the sandbox cwd and joined with a blank line. This section is present only when a sandbox is attached (`context.ts:146`, `context.ts:20-31`).
3. **`## Available Skills`**: a fixed preamble followed by one line per skill (`context.ts:148-159`). The preamble reads:
   > "The following skills provide specialized instructions for specific tasks. When a task matches a skill description, call the `activate_skill` tool with that skill name before proceeding so its full instructions are loaded. Skill instructions and supporting resources stay lazy until activation or explicit file reads."
4. **`## Available Agents`**: the subagent roster, in the same line format. This section is always present. When there are no subagents it says "None. … do not call it" (`context.ts:168-185`). The roster lives in the prompt rather than in the `task` tool, so that the tool spec can stay static (`context.ts:161-167`).
5. **`Date: <weekday, Mon D, YYYY>`**, computed with `new Date()` each time the prompt is composed (`context.ts:187-193`).
6. **`Working directory: <cwd>`** and **`Directory structure:`** (a top-level `readdir`). These appear only with a sandbox (`context.ts:194-199`).

Flue adds no behaviour of its own to the prompt (`context.ts:126-131`): "Flue adds no behavioral stance of its own (an autonomy preamble used to live here) — how an agent should behave is the agent function's instructions to give." The skill catalog merges skills declared with `useSkill` and skills discovered in the workspace under `.agents/skills/*/SKILL.md`. A name that appears in both throws (`context.ts:100-117`).

When a skill has a `result` schema, the `finish`/`give_up` footer goes into the user prompt or skill body, not the system prompt (`result.ts:16-24`).

## 2. Catalog line format

> `const desc = skill.description ? \` — ${skill.description}\` : '';`
> `parts.push(\`- **${skill.name}**${desc}\`);`
> — `context.ts:156-157`

So each line is `- **name** — description`, with an em dash, or just `- **name**` when there is no description. The resource signals reuse the same shape through `catalogLine` (`resources.ts:170-172`). Names are limited to 64 characters of `[a-z0-9]` in hyphenated segments, and descriptions to 1024 characters (`skill-definition.ts:94-105`), which follows the Agent Skills spec.

## 3. `activate_skill` and `read_skill_resource`

### `activate_skill`

- **Input:** `{ name: string }`. This is deliberately a plain string rather than an enum of skill names (`agent.ts:423-434`): "a literal union of skill names would rewrite the tool spec (and invalidate the provider's prompt cache) every time a dynamically declared skill flips."
- **Presence:** the tool is in the tool set whenever the agent has at least one live skill (`session.ts:4268-4271`).
- **Resolution:** the name is looked up in the **live** skill set, not the frozen catalog. An unknown name is not an error; it returns a plain-text miss (`session.ts:3861-3865`):
  > `` return `Skill "${name}" is not available. Available skills: ${available.join(', ')}.`; ``
- **Output** for a packaged skill: one text block (`result.ts:34-71`):
  ```
  Run the skill named "<name>".

  <skill_instructions>
  <SKILL.md body, frontmatter stripped>
  </skill_instructions>

  Supporting skill resources are available but are not loaded into context unless needed:
  <skill_resources>
  - references/x.md → read_skill_resource /.flue/packaged-skills/skill%3A<name>%3A<hash16>/references/x.md
  </skill_resources>
  ```
  The `<skill_resources>` block appears only when the skill has files besides SKILL.md, and they are listed in sorted order. The tool result `details` is `{ skill: name }` (`agent.ts:451-454`).
- A **workspace skill** re-reads SKILL.md from the sandbox on every activation, so edits made mid-session are picked up (`context.ts:42-47`, `session.ts:3866-3872`). Instead of listing files, it gives a `Base directory:` and tells the model to resolve relative paths from there (`result.ts:74-94`).

### `read_skill_resource`

- **Input:** `{ path: string, offset?: number, limit?: number }`, the same shape as the built-in `read` tool (`agent.ts:71-75`, `agent.ts:77-97`).
- **Presence:** the tool is added only when some live packaged skill has a file other than SKILL.md (`session.ts:4272-4276`).
- **Path handling:** the tool accepts the percent-encoded path it advertised, the decoded form, and the form without the leading `/`, because "Models often drop the advertised path's leading `/`" (`agent.ts:87-90`, `agent.ts:639-647`).
- **Output:** the file text, paged at 2000 lines or 50 KB with a `[Showing lines a-b of n. Use offset=… to continue.]` footer. Binary files are returned as base64 wrapped at 76 columns (`agent.ts:650-652`, `agent.ts:666-697`). An unknown path throws `Packaged skill file not found` (`agent.ts:92-93`).
- The same virtual paths also work through the ordinary sandbox `read`, `grep` and similar tools. `overlayPackagedSkills` wraps `readFile` so that `/.flue/packaged-skills/` is served from memory (`agent.ts:48-69`). Tools declared with `useTool` see the real env without this overlay.

All three framework tool names (`task`, `activate_skill`, `read_skill_resource`) are reserved. A custom tool using one of them throws `ToolNameConflictError` (`session.ts:4199-4222`).

## 4. How packaged skills and their files are stored and read

- **Build time (Vite):** `packages/vite/src/markdown-import-plugin.ts`. Any static import that resolves to a file named `SKILL.md` packages the **whole directory** (`:112-119`, `:203-223`). It collects files recursively and excludes `.git`, `node_modules`, `dist` and similar directories. Symlinks are a hard error, and so are sensitive files such as `.ssh`, `*.pem`, `*.key` and `secrets*` (`:36-46`, `:229-263`). Files over 1 MB produce a warning. The module it emits is `const directory = <JSON>; export default createSkillReference(directory);` (`:194-198`). Any other `.md` import becomes a plain string module (`:182-186`), which is how you get "always-on" skill text into the prompt with `useInstruction` (`hooks/use-skill.ts:26-27`). Dynamic `SKILL.md` imports are rejected (`:88`).
- **Shape:** `PackagedSkillDirectory { id, name, description, files: Record<path, { encoding: 'base64', kind: 'text'|'binary', content }> }` (`types.ts:224-235`). Files are sorted by path. The id is `skill:<name>:<first 16 hex chars of sha256 over (len, path, bytes)…>` (`skill-package.ts:13-39`), so it is content-addressed and changes whenever any file changes. The directory hides behind a `Symbol.for` property on a frozen `SkillReference { id, name, description }` (`skill-package.ts:41-56`).
- **Inline skills:** `defineSkill({ name, description, instructions, files?, … })` validates and freezes its input, but it packages nothing. The runtime packages the definition lazily into the same shape: it writes a SKILL.md and caches the result per object in a `WeakMap` (`skill-definition.ts:16-62`, `:187-209`).
- **Runtime:** nothing is copied into the sandbox. Files are served read-only from the in-memory map at `/.flue/packaged-skills/<encodeURIComponent(id)>/<path>` (`agent.ts:13`, `agent.ts:699-701`; docs `guide/skills.md:162`).

## 5. Why the prompt prefix stays stable, and how re-rendering affects it

**Re-rendering happens every turn.** At each turn boundary `prepareRerenderTurn` re-runs the agent function and recomposes the system prompt (`session.ts:939-1004`; `client.ts:459-472`). The new prompt replaces the content of the leading system message (`session.ts:1012-1021`). In the same step the live skill and subagent sets are swapped to whatever the new render declares (`session.ts:956-957`), and the tools array is rebuilt (`session.ts:963-973`).

**What is frozen and what is live.** The prompt is recomposed through `promptContext.recompose(next.config.instructions)`. That call reads the catalogs from a **baseline**, not from the current render (`context.ts:235-243`):

> "The catalog the prompt lists is a BASELINE snapshot, not the live skill set: dynamically declared skills announce themselves via `resources` signals instead of rewriting the prompt (which would invalidate the provider's prompt cache)."

| Prompt part | Behaviour across turns |
|---|---|
| Instruction document (returned text + `useInstruction`) | **Live**: recomposed every turn. A change is detected with an FNV-1a digest and flagged with the `instructions` signal `System instructions updated.` (`resources.ts:53-85`) |
| AGENTS.md, directory listing, cwd | Frozen at init. Re-discovered only at compaction (`session.ts:1044-1047`, `:1244-1276`) |
| `## Available Skills` | Frozen on the durable baseline, which is the first render or the stored baseline after a restart (`client.ts:426-431`) |
| `## Available Agents` | Frozen the same way (`client.ts:432-437`) |
| `Date:` | Recomputed every time the prompt is composed (`context.ts:187-193`) |

**Changes are narrated rather than rewritten.** Each render's resource snapshot is diffed against the durable *last-narrated* snapshot (`resources.ts:102-126`, `session.ts:1128-1180`). Every changed kind is appended as a signal, for example:

```
<signal type="resources" resource="skill">
New skill available:
- **refunds** — Process refund requests against the orders API.
All available skills: faq, refunds
</signal>
```

(`resources.ts:147-168`; docs `reference/agent-api.md:717-725`.) Signals reach the model as **user-role** messages (`session.ts:311-316`). They only append to the transcript, so the earlier prefix stays byte-identical. The docs give two reasons for this design. The first is cache warmth. The second is that earlier turns stay consistent with "the prompt they actually ran under" (`reference/agent-behavior.md:145-153`).

**Tools are cache-hostile, so their specs are kept static.** The `task` tool is always present and its description never changes. The roster moved into the system prompt so that "the serialized tools block never changes" (`agent.ts:387-398`, `session.ts:4277-4284`). `activate_skill` takes a plain string for the same reason. A custom tool that comes or goes still rewrites the native tools array and invalidates the cache (`reference/agent-api.md:715`).

**Compaction rebaselines.** After compaction, the prompt snapshots the then-current skill and subagent sets and re-discovers the workspace. Delta narration then starts fresh from that point (`session.ts:1231-1276`; `resources.ts:9-12`).

**Activation is cache-neutral by construction.** Skill instructions arrive as a tool result, so "the system prompt never changes when a skill activates, and the provider's cached prompt prefix survives" (docs `guide/skills.md:147`; `hooks/use-skill.ts:6-11`).

## What depends on Pi specifics

Flue runs on Pi (`@earendil-works/pi-agent-core`, `pi-ai`). The following parts are tied to it:

- **The system prompt is a transcript message.** Pi derives `AgentState.systemPrompt` from the leading `system` message, so Flue updates the prompt by replacing `messages[0]` (`session.ts:1006-1021`, `:996-999`). With a provider API that takes `system` as a separate request field, this becomes a simple field update.
- **Tool-addition announcements.** Pi's loop announces tool-set changes as system messages and keeps `toolsAdded` declarations for deferred tool loading (`session.ts:1164-1176`). The docs claim that tools added by a completed tool call stay cache-safe on current Anthropic models (`reference/agent-api.md:715`). That claim is about provider and Pi behaviour, not something Flue itself guarantees.
- **The `AgentTool` shape and TypeBox schemas** (`Type.Object`, `{ content: [{type:'text'}], details }` results). These are Pi types (`agent.ts:1-2`).
- **The `signal` message role** is Flue's own durable record type. It is lowered to a Pi `user` message at turn time (`session.ts:311-316`).
- **The Vite packaging pipeline.** It is specific to JavaScript bundling, but the idea maps onto a compile-time embed of the whole skill directory.

Everything else does not depend on Pi: the composition order, the catalog line, the tool contracts, content-addressed packaging, the virtual read path, and the frozen-baseline-plus-signals scheme.

## Notes against the current keelrs sample

`keelrs/src/skill.rs` embeds only `SKILL.md` through `include_str!` and reads `name`/`description` with a line scan. The Flue equivalents that are still missing are listed below as observations, not decisions:

- embedding the whole directory
- a content-hash id
- name and description validation
- the body without its frontmatter, for the activation result

`examples/skills/src/main.rs` already follows Flue's pattern: `use_skill(GREETING)` together with returned text that says when to activate the skill (compare `hooks/use-skill.ts:20-23`).

## Surprises

- **The docs call the prompt "frozen", but the instruction document is not.** It is recomposed every turn and sits **first** in the prompt, so any change to the returned text or to `useInstruction` invalidates the entire cached prefix. Flue accepts this and only flags it with the `instructions` signal, calling churn "a smell worth surfacing" (`resources.ts:78-85`).
- **`Date:` is recomputed on every recompose** (`context.ts:187`). When the day changes, the end of the prompt changes. Because the date sits near the end, the damage stays small.
- **`## Available Agents` is always emitted**, even when there are no subagents, because the `task` tool is always present (`context.ts:179-185`).
- **The two catalogs drift on purpose.** The frozen catalog can list a skill that no longer exists, or miss a new one. Activation checks the live set, and the signals cover the difference between the two.
