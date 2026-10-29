# How flue implements sandboxes, and what keelrs lacks

Sources: flue at `/Users/fpaschos/Dev/Work/sapp/flue` (HEAD `deee93b4`, v2.3.0) and keelrs at
`/Users/fpaschos/Dev/Work/sapp/keelrs` (branch `sandbox-listing-canonical`, `ba423ed`). flue paths
below are relative to the flue repo; `rt/` means `packages/runtime/src/`. keelrs paths are
relative to the keelrs repo. Backend names are the identifiers flue itself uses (blueprint slugs).

## Summary

- **flue and keelrs share the architecture.** In both, the agent runs on the host and the sandbox
  only gives it a filesystem and a shell. flue's `Sandbox` is a plain object with `exec` plus eight
  file verbs (`rt/types.ts:285-337`). The model never calls it directly: it goes through six
  standard tools built on top (`read`, `write`, `edit`, `bash`, `grep`, `glob`; `rt/session.ts:4313-4320`).
- **flue ships three backends in code:** `local()` (Node host), `bash()` (in-memory just-bash
  emulation) and `cloudflareSandbox()` (a container behind a Workers Durable Object). The other
  nine providers are **blueprints**: Markdown files that a coding agent uses to write an adapter
  file into the user's project (`flue add sandbox <name>`; `blueprints/sandbox.md`). All nine wrap
  a provider SDK, SSH, or CLI behind one `SandboxDriver` interface (`rt/sandbox.ts:348-366`).
- **The contract is deliberately small.** It has no teardown or lifecycle verb, no snapshot or
  fork, no ports or preview URLs, no network policy, and no streamed output: `exec` returns a
  buffered `{stdout, stderr, exitCode}`. Provisioning and deletion belong to the application
  (`apps/docs/.../reference/sandbox-api.md:44-49`).
- **Where flue invests is reliability at the edges:**
  - an abort race that releases the caller at once and reports any command still running
    ("orphan") to a callback (`rt/sandbox.ts:65-136`);
  - a `SandboxDiedError`, raised by polling the control plane while a call is pending
    (`rt/cloudflare/cf-sandbox.ts:119-170`, `rt/errors.ts:594-612`);
  - `SandboxOperationUnsupportedError`, thrown before anything changes (`rt/errors.ts:614+`);
  - per-sandbox replacement of the tool set (`rt/types.ts:1015-1042`);
  - workspace discovery: `AGENTS.md`, skills, and a directory listing read from the sandbox's cwd
    (`rt/context.ts:205`);
  - attaching or detaching the sandbox mid-run, with an `environment` signal that tells the model
    (`rt/session.ts:1023-1100`).
- **keelrs's `SandboxBackend` already matches or exceeds flue's file and exec contract.** It
  confines paths, writes atomically, has `write_if`, streams `exec` output, has an exec kill
  handle, `capabilities`, and `close`. flue's `local()` does none of these.
- **The keelrs gaps are above the contract and in its breadth:**
  - no backend except `local()`;
  - no standard sandboxed tool set (issue #42; `examples/dogfood/src/coding.rs:1-2`);
  - no factory keyed on the agent instance, so a durable workspace has to be built by hand;
  - no death detection for remote backends;
  - no workspace discovery from inside the sandbox;
  - no in-memory sandbox.

## flue sandbox contract

### The three layers

| Layer | What it is | Where |
|---|---|---|
| `SandboxFactory` | `{ createSandbox({id}): Promise<Sandbox>, tools?: SandboxToolFactory }`. Lazy: the value is cheap to build, and `createSandbox` runs once per initialized harness with the agent instance id. | `rt/types.ts:1015-1042`; `rt/hooks/use-sandbox.ts:4-44` |
| `Sandbox` | The live surface the runtime and tools use: `exec`, `readFile`, `readFileBuffer`, `writeFile`, `stat`, `readdir`, `exists`, `mkdir`, `rm`, `cwd`, `resolvePath`. | `rt/types.ts:285-337` |
| `SandboxDriver` | What a provider adapter implements: the same verbs minus `cwd`/`resolvePath`. `sandboxFromDriver(driver, cwd, {onOrphanSettled})` turns it into a `Sandbox`, adding path resolution, `writeFile` parent creation and the abort race. | `rt/sandbox.ts:348-366`, `rt/sandbox.ts:379-459` |

There is also `bash(factory)` for any just-bash-like object (`BashLike`, `rt/types.ts:1047-1070`;
wrapper at `rt/sandbox.ts:226-285`).

### Methods

| Capability | flue semantics | Citation |
|---|---|---|
| `exec(command, {cwd, env, timeoutMs, signal})` | Takes a shell string only (no argv form). Resolves with a `ShellResult` for any exit; rejects only on transport failure or abort. `timeoutMs` is the main way to cancel and is forwarded to the provider's own timeout (rounded up, never down). `signal` cancels on demand. | `rt/types.ts:286-317`, `rt/types.ts:990-994`, `apps/docs/.../reference/sandbox-api.md:98-107` |
| Abort / orphan handling | On abort the caller is rejected at once with `AbortError`. If the provider call is still in flight it becomes an orphan: its eventual result is consumed and reported only to `onOrphanSettled`, never recorded in the conversation. | `rt/sandbox.ts:17-136` |
| Timeout as a result | The `bash` tool turns a model timeout into a recoverable result with `exitCode: 124`. A host abort is rethrown instead. | `rt/agent.ts:287-346` |
| Output limits | The `local` backend keeps at most 64 MiB of output and kills the process tree beyond that. Tools show the model the last 2000 lines or 50 KB. | `rt/node/local-env.ts:17,115-142`; `rt/agent.ts:7-11,292` |
| Read | `readFile` (UTF-8) and `readFileBuffer` (bytes). Always the whole file; the `read` tool slices it in the runtime. | `rt/types.ts:319-320`; `reference/sandbox-api.md:335` |
| Write | `writeFile` must create missing parents. The shared helper tries the write, runs `mkdir -p` on failure, and retries once. The write is not atomic. | `rt/sandbox.ts:150-166` |
| `stat` | Returns `{isFile, isDirectory, isSymbolicLink?, size?, mtime?}` with the target's metadata (`stat -L` semantics). Adapters must not fabricate missing fields. | `rt/types.ts:268-274`; `reference/sandbox-api.md:132-145` |
| `readdir` / `exists` / `mkdir` / `rm` | `readdir` returns entry names only. `exists` never throws. `rm` supports `recursive`/`force`, or must throw `SandboxOperationUnsupportedError` before changing anything. | `rt/types.ts:322-326`; `reference/sandbox-api.md:109-118` |
| Path scoping | `useSandbox(f, {cwd})` wraps the sandbox with `createCwdSandbox`: POSIX-normalized resolution against the cwd. This is **not confinement**: `..` and absolute paths pass through. | `rt/sandbox.ts:173-220`; `rt/client.ts:369-372` |
| Env | `local()` keeps 14 allowlisted host vars, then layers `options.env`, then the per-call `env`. Env values are redacted from transcripts (only the keys are recorded). | `rt/node/local-env.ts:163-235`; `rt/shell.ts:46-55` |
| Liveness | Adapters should settle in-flight calls when the sandbox dies, for example by polling the control plane, and reject with `SandboxDiedError`. There is deliberately no per-command deadline. | `rt/sandbox.ts:334-347`; `rt/cloudflare/cf-sandbox.ts:72-170` |
| Unsupported operations | `SandboxOperationUnsupportedError {operation, provider, options}`. | `rt/errors.ts:614+`; `reference/sandbox-api.md:403-411` |
| Native extension | An adapter may return extra properties, reached through a checked accessor (e.g. `computerWorkspace(harness.sandbox)`). A `cwd` override drops them. | `reference/sandbox-api.md:147-152` |
| Tool set override | `SandboxFactory.tools(sandbox, {subagents})` **replaces** the six standard tools (`task` and the skill tools are still added). It is composed from the exported `createReadTool`…`createGlobTool`. | `rt/types.ts:1005-1041`; `rt/session.ts:4295-4325` |
| `grep` / `glob` | `grep` checks once whether `rg` is available and falls back to `grep -rnH`. `glob` runs `find -name`. Both need `exec`. | `reference/sandbox-api.md:333-338` |
| Packaged skills | `readFile` is overlaid only for the tools, so `/.flue/packaged-skills/...` is served from the application bundle and never written into the sandbox. | `reference/sandbox-api.md:340-348` |
| **Not in the contract** | Teardown or dispose, snapshot or fork, ports or preview URLs, network egress policy, streamed stdout, PTY, background processes, file watching. | `reference/sandbox-api.md:44-49`; a grep across `rt/` and `blueprints/` finds none |

## Backends

| Backend | How it ships | Isolation | Transport and implementation notes | Cancellation | Death detection | Citation |
|---|---|---|---|---|---|---|
| `local()` | Built in, `@flue/runtime/node` | None (the host itself) | Host `fs` plus `child_process.spawn` through an absolute bash path. Each command leads its own process group. On abort: SIGTERM, then SIGKILL after 2 s. Exit code 124 on a timeout kill. | Real (kills the process group) | n/a | `rt/node/local.ts:18-22`; `rt/node/local-env.ts:36-152,237-331` |
| `bash()` (virtual) | Built in. The user supplies a `just-bash` instance. | In-process emulation: the filesystem is in memory, no real processes run, network is opt-in by URL prefix. It is ephemeral. | Duck-typed `BashLike`; `timeoutMs` becomes a signal. | Cooperative | n/a | `rt/sandbox.ts:222-307`; `guide/sandboxes.md:23-46` |
| `cloudflareSandbox()` | Built in, `@flue/runtime/cloudflare`. Works only inside a Worker. | Container behind a Durable Object | `SandboxDriver` over an RPC stub. `stat`, `readdir` and `rm -r/-f` are shell commands. Binary files go as base64. Default cwd is `/workspace`. | None (signal not forwarded; orphaned) | Polls `getState()` every 5 s, gives up after 10 s of silence | `rt/cloudflare/cf-sandbox.ts:15-301`; `blueprints/sandbox--cloudflare.md:17-30` |
| `cloudflare-computer` | Blueprint | Durable virtual filesystem stored in SQLite inside the agent's Durable Object; just-bash running in a Dynamic Worker | Exposes a native `Workspace` (with a git client) through an accessor. The blueprint calls it an early preview. | Best-effort `run.kill('SIGKILL')` | — | `blueprints/sandbox--cloudflare-computer.md:18-30,179-240` |
| `e2b` | Blueprint (SDK) | Firecracker microVM, up to 24 h | `commands.run`, `files.*`. A non-zero exit throws `CommandExitError` and the adapter maps it to a result. `rm` flags are unsupported. | `timeoutMs` only | — | `blueprints/sandbox--e2b.md:21-23,98-196,304` |
| `daytona` | Blueprint (SDK) | Remote sandbox | `process.executeCommand`, `fs.uploadFile`, `fs.deleteFile(path, recursive)`. | `timeout` only | Polls `refreshData()` every 5 s | `blueprints/sandbox--daytona.md:79-160,181-286` |
| `modal` | Blueprint (SDK) | Container | `exec` returns a process with stdout/stderr streams; the adapter drains both. Whole-file filesystem API; `stat`, `mkdir` and `rm` run in the shell (handles BusyBox `stat`). | `timeout` only | Status polling | `blueprints/sandbox--modal.md:23-43,118-377` |
| `vercel` | Blueprint (SDK) | microVM sandbox | `AbortSignal.any([caller, timeout])` forwarded to the SDK and to the stdout/stderr readers. | **Real** | — | `blueprints/sandbox--vercel.md:75-181` |
| `boxd` | Blueprint (SDK) | microVM with persistent disk; fast forks | Exec rides a bidirectional gRPC stream that only settles when the server ends it. `stat` and friends run in the shell. | `timeout` only | Status polling (needs the `Compute` client) | `blueprints/sandbox--boxd.md:22-31,105-255,405` |
| `exedev` | Blueprint (SSH) | Remote VM | `ssh2`: `exec` over an SSH channel, files over SFTP streams. Optional lifecycle helpers `createExeVm`, `cloneExeVm`, `deleteExeVm` over HTTPS. Node only. | — | — | `blueprints/sandbox--exedev.md:17-24,115,252-272,570-704` |
| `islo` | Blueprint (CLI) | Remote sandbox | Spawns the local `islo` binary for every operation. No SDK; Node only. `islo pause` with automatic resume. | None | — | `blueprints/sandbox--islo.md:23-30,91-233,290` |
| `mirage` | Blueprint (SDK) | Virtual workspace of mounted resources (RAM, SSH, Postgres, …) | Node and browser packages share one `Workspace` API. | **Real** (cooperative signal) | — | `blueprints/sandbox--mirage.md:18-33,114-273` |
| Generic | Blueprint `sandbox.md` | Any | A coding agent writes an adapter against the `SandboxDriver` spec, with daytona as the worked example. | — | — | `blueprints/sandbox.md:1-90` |

How the blueprints work: each one is versioned (`// flue-blueprint: sandbox/<name>@1`) and lives in
the user's project as `<src>/sandboxes/<name>.ts`. It imports only from `@flue/runtime`. `flue
update` re-applies upgrade guides (`blueprints/sandbox.md:17-60`; CLI in
`packages/cli/src/commands/blueprints.ts`).

## Lifecycle and server layer

- **Declaration.** `useSandbox(factory, {cwd})` may be called at most once per render. Calling it
  in a subagent render is an error, because subagents share the parent's sandbox. The call may be
  conditional (`rt/hooks/use-sandbox.ts:45-90`).
- **Creation.** Once per submission's harness init, `resolveSandbox(id, factory)` calls
  `createSandbox({id})`. It runs in parallel with MCP connection, then the cwd wrapper is applied
  and the workspace is discovered (`rt/client.ts:355-389`, `rt/client.ts:579-606`). The `id` is the
  agent instance id. Factories key provider resources on it to give each conversation a durable
  workspace, and must tolerate being called again with the same id (`rt/types.ts:1016-1024`;
  `guide/sandboxes.md:170`; `guide/durability.md:146-150`).
- **Ownership.** flue never creates, reuses or destroys provider infrastructure itself. There is
  no dispose hook; the application owns the provider sandbox (`reference/sandbox-api.md:46`).
- **Swapping.** Only whether `useSandbox` is present is watched at turn boundaries. When it flips,
  the session resolves the new factory or detaches, rebuilds the tools, re-runs skill discovery,
  and emits one `environment` signal that restates the cwd plus the tool, skill and subagent
  rosters. The system prompt stays frozen until the next compaction (`rt/session.ts:1023-1100`;
  `guide/sandboxes.md:178-214`).
- **Use outside the model.** `harness.sandbox` is the raw sandbox for host code and is never
  recorded (`rt/types.ts:735-777`). `session.shell()` is recorded as a `bash` tool exchange with
  env values redacted (`rt/shell.ts:21-60`). `session.fs` is unrecorded (`rt/types.ts:351-400`).
- **Server and runtime.** These layers carry no sandbox-specific state:
  - The Node target has an agent coordinator plus an SQL-backed submission store with leases
    (`rt/node/agent-coordinator.ts`, `rt/node/agent-execution-store.ts`).
  - The Cloudflare target runs one Durable Object per agent (`rt/cloudflare/agent-coordinator.ts`,
    `rt/cloudflare/flue-agent-class.ts`).
  - HTTP routes are in `rt/runtime/*`.
  - The sandbox is rebuilt from the factory on every submission and recovery attempt, so only an
    id-keyed sandbox persists across them (`guide/durability.md:148-150`).
  - The Cloudflare coordinator documents sandbox RPCs that ignore the abort signal as a known
    hang source (`rt/cloudflare/agent-coordinator.ts:69,857`).
- **Doc inconsistency.** `guide/cloudflare-target.md:194` says agents get an in-memory sandbox by
  default, but the code has no default: `rt/hooks/use-sandbox.ts:24-27` and
  `rt/client.ts:575-585` both leave the agent with no sandbox.

## Gap table vs keelrs

keelrs references: `keelrs/src/sandbox/mod.rs` (trait at `:230-315`, handle at `:325-560`, exec
types at `:576-780`), `keelrs/src/sandbox/local.rs`, `keelrs/src/agent/context.rs:46-56,118-135`,
`docs/sandbox.md`.

| flue capability | keelrs | What it would take in Rust |
|---|---|---|
| `exec` with shell string, cwd, env, timeout, cancel | **Has, and more.** `ExecRequest` supports a shell line or argv, `cwd`, env set/remove, `stdin`, `timeout` and `merge_stderr` (`mod.rs:576-670`). `Execution` streams `Chunk`s and has `kill()`; dropping it kills the command (`mod.rs:708-773`). | — |
| Exit-code 124 on timeout | **Has, differently.** `Exit::TimedOut`, `Signaled` and `Killed` are distinct (`mod.rs:684-695`). | — |
| Orphan reporting (`onOrphanSettled`) | **Partial.** `Sandbox::race` makes calls fail with `Died` after `close` (`mod.rs:544-555`), but a remote command left running is not reported anywhere. | Add an optional orphan callback, or a `tracing` event, at the point where a remote backend drops the waiting caller. Only needed once a remote backend exists. |
| File verbs | **Has, and more.** `stat`, `read` (limit plus size), atomic `write`, `write_if` (compare by hash), `remove`, `rename`, recursive `list` with glob and ignore rules, `canonical`/`canonical_all`, batched `read_files`. Missing: `mkdir` (`write` creates parents), separate `exists` (`Stat::Missing`), and a `readdir` that returns names. | Add `mkdir` only if a tool needs empty directories. |
| Path scoping | **Has, and stronger.** `Sandbox::place` canonicalizes every path and refuses anything outside the work dir; `work_dir()` narrows it (`mod.rs:361,522-541`). flue's `local()` does not confine paths. | — |
| Env allowlist | **Has.** Allowlist plus `LC_*` and `GIT_TERMINAL_PROMPT=0`, then builder overrides (`local.rs:114-118`). | Redact env values in events, as flue does, when keelrs records tool arguments. |
| Unsupported-operation error | **Has.** `SandboxError::Unsupported` plus `capabilities()`, checked before the call (`mod.rs:116-160,557`). | — |
| Sandbox death (`SandboxDiedError`) | **Partial.** `SandboxError::Died` exists, but nothing detects death. | A per-backend liveness probe that `Sandbox::race` selects against, polling the control plane every 5 s and treating 10 s of silence as death, the same shape as `cf-sandbox.ts:119-170`. Fits the existing `CancellationToken` race. |
| Teardown | **Has (flue lacks it).** `close()` in the trait (`mod.rs:311`). | Keep. Under ADR 0005 the application owns the sandbox, which matches flue's rule that flue never destroys infrastructure. |
| Lazy factory keyed on the instance id | **Lacks.** `use_sandbox(Sandbox)` takes a live handle (`context.rs:53`). The glossary's **Sandbox key** has no API yet. | A `SandboxFactory` trait (`async fn open(&self, key) -> Result<Sandbox>`) or a runtime-side registry keyed by Sandbox key, so many agents can share one sandbox and recovery finds the same one. |
| Mid-run swap plus `environment` signal | **Partial.** The sandbox is declared on every render and pinned per turn with the tools (`agent/rig/mod.rs:92-94`), but nothing tells the model when it changes. | Emit a keelrs event and a model-visible note when the sandbox id or work dir changes between turns. |
| Standard tool set (`read`/`write`/`edit`/`bash`/`grep`/`glob`) | **Lacks** (issue #42). The dogfood example wraps rig-tools, which read the host disk directly, so they are only right for `local()` (`examples/dogfood/src/coding.rs:1-8,81-100`). | Built-in sandboxed tools written only against `Sandbox`: `read` (via `read`), `write`/`edit` (via `write_if`), `list`/`search` (via `list` plus `read_files`, so no `rg` needed), `shell` (via `exec`). Per-path locks keyed on the canonical path. Per-sandbox replacement of the tool set. |
| Workspace discovery (AGENTS.md, skills, listing from the sandbox cwd) | **Lacks.** Skill discovery reads the host disk before the runtime starts (`skills/discover.rs`). | Read `AGENTS.md` and `.agents/skills/*/SKILL.md` through `Sandbox::read_files` and `list` at dispatch start. |
| In-memory virtual sandbox | **Lacks.** | A `memory()` backend over a `BTreeMap<SandboxPath, Vec<u8>>` with `capabilities().exec = false`, which also serves tests. For an emulated shell, `brush-core` 0.5.0 exists (a reusable bash core, github.com/reubeno/brush) but would need a virtual-filesystem shim, so leave it for later. |
| Container backend | **Lacks** (`docker()` is planned in `docs/sandbox.md`). | `bollard` 0.21.1 (async Docker API; verified). Exec via `create_exec`/`start_exec` with streamed output. Files as tar through `upload_to_container`/`download_from_container`, or `cat`/`stat` through exec. `canonical_all` as one `realpath -m` exec. |
| SSH / VM backend (`exedev` shape) | **Lacks.** | `russh` 0.65.0 plus `russh-sftp` 3.0.1 (both verified): exec over a channel, files over SFTP. This one backend covers any VM, including a microVM reached by SSH. |
| microVM backends | **Lacks.** | Local options: `microsandbox` 0.7.11 (verified, github.com/superradcompany/microsandbox) and `firepilot` 1.2.0 (Firecracker control; verified). Most hosted microVM services in flue's list have no maintained Rust SDK; reach them over HTTP with `reqwest` or over SSH. |
| Hosted provider backends (e2b, daytona, modal, vercel, …) | **Lacks.** | Rust SDKs are immature: `e2b-sdk` 0.1.1 (third-party, last update 2025-02); `daytona` is only `0.1.0-alpha.1`; the `modal` crate is unrelated. Plan to call provider REST APIs directly behind a `SandboxBackend`. |
| Blueprint / code-generated adapters | **Lacks** (it is a distribution pattern). | Optional. An agent skill holding a "write a `SandboxBackend` for provider X" brief plus the contract would mirror `blueprints/sandbox.md`. |
| Snapshot, ports, network policy, PTY | **Neither has it.** | Out of scope for parity. ADR 0005 already plans network control through proxies. |

## Recommendations

1. **Keep keelrs's contract.** It is already stronger than flue's `SandboxDriver`: confinement,
   atomic and conditional writes, streamed exec, `close`. Do not copy `readdir`, `exists` or the
   `mkdir`-retry write.
2. **Next, ship built-in sandboxed tools (#42) written only against `Sandbox`.** That gives every
   future backend the full tool set for free (flue's main win, `rt/session.ts:4313-4320`), and
   removes the rig-tools stand-in from the examples, which helps the AGENTS.md example metric.
3. **Add a `memory()` backend next.** It is cheap and gives a backend-independent test of the
   tools and `Sandbox`, plus an exec-less example. It also validates `capabilities`.
4. **Then add `docker()` on `bollard`.** It is the first backend with real round trips, so it
   exercises `canonical_all` and `read_files` batching. Add the liveness probe (flue's polling
   shape) inside `Sandbox::race` at the same time.
5. **Add an `ssh()` backend on `russh` and `russh-sftp`** as the generic remote option. It covers
   VMs and microVMs without per-provider SDKs, matching flue's `exedev`.
6. **Design the Sandbox key / factory seam before any remote backend.** For example: the runtime
   opens a sandbox by key at dispatch start and reuses it for every agent with that key. Without
   it, durable workspaces and recovery reattachment have to be hand-built in each application.
7. **Later:**
   - workspace discovery through the sandbox;
   - an `environment` note when the declared sandbox changes between turns;
   - redacted env values in tool events.
