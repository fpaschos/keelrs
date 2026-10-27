# Sandboxes in Flue and Rig, and the seams a keelrs `Sandbox` needs

Research for the keelrs sandbox API (`keelrs/src/sandbox.rs` is an empty placeholder today). It records how Flue's `Sandbox` and Rig's `Workspace` are built, read from source. Sections 1 and 2 are facts with citations. Section 3 compares them. **Section 4 is inference**: the seams a keelrs `Sandbox` trait would need so that container, microVM and remote backends fit later. It makes no decisions.

This builds on two earlier notes and does not repeat them:

- [`rig-tools.md`](https://github.com/fpaschos/keelrs/blob/research/rig-tools/docs/research/rig-tools.md) (branch `research/rig-tools`): Rig's `Tool`, `ToolContext`, `ToolSet`, and the point that `Tool::footprint` / `Touches` exists but the `rig-agent` loop does not consult it.
- [`rig-tools-rc1.md`](https://github.com/fpaschos/keelrs/blob/research/rig-tools-rc1/docs/research/rig-tools-rc1.md) (branch `research/rig-tools-rc1`): the tool API is unchanged on `0.46.0-rc.1`, and `ToolContext` carries non-serialised `scope`s (`with_scope(Arc<dyn Any + Send + Sync>)`), through which a host can hand a tool a live handle, such as a sandbox.

House rule: this note names only keelrs, Rig and Flue. Third-party sandbox providers, container and microVM products, and libraries are described generically. Where a source path contains a vendor name, it is written as `<edge>` or `<provider>`.

**Sources.**

- Flue: local checkout `/Users/fpaschos/Dev/Work/sapp/flue` at tag `v2.3.0` (commit `deee93b4`, 2026-10-12). `FLUE/` below stands for `packages/runtime/src/` in it. `DOCS/` stands for `apps/docs/src/content/docs/`.
- Rig: tag [`v0.46.0-rc.1`](https://github.com/0xPlaygrounds/rig/tree/v0.46.0-rc.1), extracted with `git archive v0.46.0-rc.1`. `crates/rig-tools/src` is byte-identical (`diff -r`) to the published `rig-tools-0.46.0-rc.1` crate in `~/.cargo/registry`. `TOOLS/`, `HARNESS/` and `CORE/` stand for `https://github.com/0xPlaygrounds/rig/blob/v0.46.0-rc.1/crates/{rig-tools,rig-harness,rig-core}/src`. The local checkout is 21 commits past rc.1. Those commits change `rig-tools` (shell, read, a `Listing::from_options` helper), but not the shape of the `Workspace` trait.
- keelrs: `GLOSSARY.md`, `keelrs/src/agent/context.rs` and `keelrs/Cargo.toml` at `main` (`3bf76e3`).

## Summary

- **Flue: one async interface, `Sandbox`, and the model never touches it directly.** It has 8 file verbs, `exec(command, {cwd, env, timeoutMs, signal}) -> Promise<ShellResult>`, a `cwd` string and `resolvePath`. Every method returns a Promise. Content is `string | Uint8Array`, and paths are POSIX strings. Adapters usually implement a smaller `SandboxDriver` and let `sandboxFromDriver` add path resolution, the abort race and parent-directory creation.
- **Rig: one synchronous trait, `Workspace`.** It has 10 required or defaulted file methods (stat, read, excerpt, write, replace, write_if, remove, rename, files, watch), `canonical`/`join` for paths, and `spawn(...) -> io::Result<Process>`. `Process` wraps a **local** `std::process::Child`, so a remote workspace runs a command as a local transport process. Tools call the trait inside `blocking(...)` (one OS thread per call).
- **Backends.** Flue ships three first-party sandboxes: an in-memory emulated bash (`bash(factory)`), the host (`local()`), and one hosted container sandbox on an edge platform. A further ten providers are **blueprints**: prompts that have a coding agent write an adapter file into the user's project. They are not packages. Rig ships `Local`, `Overlay` (keeps writes in memory and refuses `spawn`), `Remote` (any command-line transport, such as ssh, with one `sh` script per file operation), and `Hosts` (routes `HOST:PATH` paths to a `Remote`).
- **Lifecycle.** Flue: `SandboxFactory.createSandbox({id})` is called once per initialised harness, keyed on the agent instance id, and there is **no teardown verb**. Provisioning and deletion belong to the application. Rig: no create or destroy at all. The workspace is a value (`ProjectWorkspace(Arc<dyn Workspace>)`, one per app), and the only lifecycle is per process: its own process group, SIGTERM, then SIGKILL after 2 s, also on drop.
- **Tool binding.** Flue's built-in `read/write/edit/bash/grep/glob` exist only when a sandbox is declared. A factory's `tools` can replace them. Custom tools reach the sandbox through `harness: true`. Rig's tools are structs with a public `workspace: Arc<dyn Workspace>` field (`Default` is `Local`), plus `base` and `confine`, so they always exist. rig-harness rebuilds them for each call from a `CallPlace`.
- **Safety.** Neither confines processes. Flue's `local()` has "no isolation, by design" but filters the environment through an allowlist. Rig's `confine` refuses file-tool paths outside `base`, and the docs say plainly that it "is no process sandbox". Both cut tool output at 2000 lines or 50 KB. Both kill runaway output (Flue at 64 MiB in `local`, Rig at 10 MB in `shell`). Only Rig declares footprints (`Touches`). Flue serialises same-file writes with a per-sandbox lock.

## 1. Flue

### 1a. The abstraction: `Sandbox`, `SandboxDriver`, `SandboxFactory`

`Sandbox` is "the universal environment interface. All sandbox modes (isolate, local, remote) implement this" (`FLUE/types.ts#L278-L337`):

```ts
export interface Sandbox {
	exec(
		command: string,
		options?: {
			cwd?: string;
			env?: Record<string, string>;
			timeoutMs?: number;   // deadline hint, forwarded to the provider's native timeout
			signal?: AbortSignal; // mid-flight cancel; rejects promptly, may orphan the command
		},
	): Promise<ShellResult>;

	readFile(path: string): Promise<string>;
	readFileBuffer(path: string): Promise<Uint8Array>;
	/** Creates missing parent directories (the `FlueFs.writeFile` guarantee). */
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	stat(path: string): Promise<FileStat>;
	readdir(path: string): Promise<string[]>;
	exists(path: string): Promise<boolean>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	rm(path: string, options?: { recursive?: boolean; force?: boolean }): Promise<void>;

	cwd: string;
	resolvePath(p: string): string;
}
```

Its value types (`FLUE/types.ts#L268-L274`, `#L975-L994`):

```ts
export interface FileStat { isFile: boolean; isDirectory: boolean; isSymbolicLink?: boolean; size?: number; mtime?: Date; }
export interface ShellResult { stdout: string; stderr: string; exitCode: number; }
export interface ShellOptions { env?: Record<string, string>; cwd?: string; timeoutMs?: number; signal?: AbortSignal; }
```

- **Everything is async, except `cwd` and `resolvePath`.** Output is buffered: `exec` resolves once, with whole `stdout`/`stderr` strings. Nothing is streamed.
- **Paths are POSIX strings**, absolute or relative to `cwd`. "Use absolute paths for portability across sandbox adapters" (`FLUE/types.ts#L355-L358`).
- **`FileStat` fields are optional**, so that an adapter "must never fabricate placeholder values" (`FLUE/types.ts#L262-L267`).
- **`exec` cancellation has two channels.** `timeoutMs` is "the **primary** cancellation contract", forwarded to the provider's native timeout. `signal` is optional mid-flight cancellation, which most provider SDKs cannot honour (`FLUE/sandbox.ts#L309-L347`).

**`SandboxDriver`** is the smaller surface that adapter authors implement: the same 8 file verbs plus `exec`, but no `cwd`/`resolvePath` (`FLUE/sandbox.ts#L348-L366`). `sandboxFromDriver(driver, cwd, { onOrphanSettled? }): Sandbox` (`FLUE/sandbox.ts#L379-L459`) adds three things:

- **Path resolution against `cwd`.** POSIX-normalised, with `..` folded by text (`#L173-L195`).
- **Parent creation for `writeFile`.** It is lazy: try the write, and on failure `mkdir -p` the parent and retry once (`writeFileCreatingParents`, `#L150-L166`).
- **The abort race.** On abort the promise rejects at once with an `AbortError`, whose message ends "The sandbox command could not be confirmed cancelled and may still be running." The provider promise becomes an orphan. Its eventual settlement goes only to `onOrphanSettled` (`OrphanedExecSettlement {command, startedAt, abortedAt, settledAt, result?, error?}`, `#L29-L42`, `#L65-L136`). "Adapters must not implement their own abort race" (`#L328-L330`).

**`SandboxFactory`** is what an agent declares (`FLUE/types.ts#L1014-L1041`):

```ts
export interface SandboxFactory {
	createSandbox(options: { id: string }): Promise<Sandbox>;
	createSessionEnv?(options: { id: string }): Promise<Sandbox>; // @deprecated
	tools?: SandboxToolFactory;
}
export type SandboxToolFactory = (sandbox: Sandbox, options: SandboxToolFactoryOptions) => AgentTool<any>[];
export interface SandboxToolFactoryOptions { subagents: Record<string, SubagentDefinition>; }
```

**Errors** (`FLUE/errors.ts`):

- `SandboxDiedError({operation, reason: 'stopped' | 'probe_silent'})` (`#L580-L611`) is raised when a sandbox dies with a call in flight. It is "Deliberately not an `AbortError`".
- `SandboxOperationUnsupportedError({operation, provider, options})` (`#L613-L632`) is thrown "before the filesystem was modified" when an adapter cannot honour an option such as `rm {recursive}`.
- Other failures are plain `Error`s. "Errors thrown by file verbs surface to the model as tool errors, so messages should be factual and self-contained" (`DOCS/reference/sandbox-api.md#L120`).

**Native surfaces.** "An adapter may return a sandbox with additional properties — a native surface beyond the generic verbs", reached by a runtime-checked accessor. "A sandbox that cannot execute commands should still ship all file verbs and throw from `exec`" (`DOCS/reference/sandbox-api.md#L149-L152`). The `cwd`-scoping wrapper drops those extra properties (`DOCS/reference/sandbox-api.md#L51-L57`).

### 1b. Backends

First-party, in `@flue/runtime`:

| Backend | Entry | How it implements `Sandbox` |
| --- | --- | --- |
| Virtual (in-memory FS plus emulated bash) | `bash(factory: BashFactory): SandboxFactory` (`FLUE/sandbox.ts#L226-L285`) | Wraps a structural `BashLike {exec, getCwd, fs{…, resolvePath(base, path)}}` (`FLUE/types.ts#L1043-L1070`). It comes from a third-party in-process bash emulator that the user adds as a dependency, so no process is spawned. `timeoutMs` becomes an `AbortSignal`. |
| Host | `local(options?: LocalSandboxOptions): SandboxFactory` from `@flue/runtime/node` (`FLUE/node/local.ts#L18-L22`) | `createLocalSandbox` (`FLUE/node/local-env.ts#L237-L331`): files through the host fs, `exec` through `spawn(command, {shell: bash-if-found, detached: true})`. |
| Hosted edge container | `<edge>Sandbox(stub, {cwd?})` from `@flue/runtime/<edge>` (`FLUE/<edge>/…-sandbox.ts#L58-L65`) | A `SandboxDriver` over a structural RPC stub (`exec`, `readFile`, `writeFile`, `exists`, `mkdir`, `deleteFile`, `getState`; `#L15-L36`). `stat`, `readdir` and flagged `rm` run as shell commands (`stat -L -c`, `find -printf`, `rm --`). Binary content travels as base64. Default cwd `/workspace` (`#L174-L301`). |

Not first-party: **ten provider blueprints** (`blueprints/sandbox--*.md`, plus a generic `blueprints/sandbox.md`). A blueprint is a prompt for a coding agent, run by `flue add sandbox <url>`. The agent writes `<src>/sandboxes/<name>.ts` into the user's project, implementing `SandboxFactory`/`SandboxDriver` against `@flue/runtime` only (`blueprints/sandbox.md#L44-L95`). `examples/hello-world/src/sandboxes/<provider>.ts` is a worked example: the user creates the provider sandbox with the provider's SDK and wraps it.

### 1c. Lifecycle and scope

- **Creation.** `createSandbox({id})` is "Called once per initialized harness — one call per `init()` — and every session and task session of that harness shares the returned sandbox". `id` is the agent instance id, and "a sandbox adapter that keys provider resources on `id` must tolerate repeated calls with the same value" (`FLUE/types.ts#L1016-L1025`). The runtime calls it while it initialises a submission, in parallel with MCP connects (`resolveSandbox`, `FLUE/client.ts#L360-L375`, `#L579-L605`). "Keying a provider workspace on `id` is how a conversation gets a durable filesystem across messages and restarts" (`DOCS/reference/sandbox-api.md#L25`).
- **No teardown.** "There is no `dispose()` or lifecycle callback. Flue … never creates, reuses, or destroys provider infrastructure on its own … An adapter must not call the provider's `delete()`/`terminate()`/`kill()` on the application's behalf" (`DOCS/reference/sandbox-api.md#L46`). Also "No per-message rebuild" and "No identity beyond `id`" (`#L47-L49`).
- **Scope.** One sandbox per initialised harness, shared by its sessions. Subagents cannot declare one: `useSandbox()` throws in a subagent render, because "Delegates share the parent agent's environment (scope work with the task call's cwd instead)" (`FLUE/hooks/use-sandbox.ts#L52-L56`). A task child gets `createCwdSandbox(parentEnv, parentEnv.resolvePath(cwd))` (`FLUE/harness.ts#L329-L333`, `FLUE/sandbox.ts#L197-L220`).
- **Swaps.** Presence of `useSandbox()` is re-read at each turn boundary. A flip attaches or detaches the environment, re-runs workspace discovery, and announces one `environment` signal to the model. Replacing sandbox A by B while it stays attached waits for the next submission (`FLUE/session.ts#L1031-L1055`; `DOCS/guide/sandboxes.md#L212-L214`).
- **Processes (host).** Each `exec` leads a new process group (`detached: true`). On abort or timeout, `process.kill(-pid, 'SIGTERM')`, then SIGKILL after `KILL_GRACE_MS = 2000`. The pending SIGKILL "stays armed" after the shell exits, for group members that ignore SIGTERM (`FLUE/node/local-env.ts#L19-L20`, `#L61-L152`). A spawn failure resolves as `exitCode: 1`. A self-initiated kill resolves as `124`.
- **Remote liveness.** "An adapter should ensure in-flight operations settle when the sandbox dies". The edge adapter polls a cheap `getState()` every 5 s while a call is pending, and presumes the sandbox dead after 10 s of probe silence. "There is deliberately no deadline" (`FLUE/sandbox.ts#L334-L346`; `FLUE/<edge>/…-sandbox.ts#L70-L160`).

### 1d. How tools bind to the sandbox

- **Built-ins exist only with a sandbox.** `createBuiltinToolGroups` returns only the framework tools (`task`, `activate_skill`, packaged-skill read) when there is no environment. With one, it returns `read, write, edit, bash, grep, glob` built over it, or the factory's own `tools(toolEnv, {subagents})` instead (`FLUE/session.ts#L4285-L4326`). From the hook docs: "Without it, the agent has NO environment: the built-in shell and filesystem tools aren't added, and sandbox-backed operations throw. There is no default sandbox." (`FLUE/hooks/use-sandbox.ts#L24-L27`).
- **Per-tool factories are exported** so a `tools` list can be composed: `createReadTool(env: Sandbox): AgentTool<…>`, and likewise `createWriteTool`, `createEditTool`, `createBashTool`, `createGrepTool`, `createGlobTool` (`FLUE/agent.ts#L104`, `#L180`, `#L220`, `#L287`, `#L524`, `#L589`). `read`/`write`/`edit` need only file verbs. **`grep` and `glob` run through `exec`**: `rg` or `grep -rnH`, and `find … | head -1000` (`FLUE/agent.ts#L524-L600`). An exec-less sandbox therefore loses them too.
- **The packaged-skill overlay.** Tool factories get `overlayPackagedSkills(env, …)`, which answers `readFile` for `/.flue/packaged-skills/…` from memory and delegates every other call (`FLUE/agent.ts#L46-L69`). `harness.sandbox` and `useTool` handlers see the real env (`DOCS/reference/sandbox-api.md#L342-L348`).
- **Custom tools.** A `ToolDefinition` with `harness: true` gets `harness`, whose `harness.sandbox` is the live `Sandbox` (`FLUE/tool-types.ts#L31-L38`, `#L116-L124`). The getter **throws** when no sandbox is declared (`FLUE/harness.ts#L112-L120`). Tools without `harness` "are pure functions of their data".
- **Programmatic shell.** `session.shell(command, ShellOptions)` runs `env.exec` inside the `bash` tool-event envelope and records it in the transcript. Env **values** are redacted to `<redacted>` in events (`FLUE/shell.ts#L29-L117`, `#L132-L134`). `session.fs` / `harness.sandbox` operations are "never recorded in a conversation" (`FLUE/types.ts#L343-L354`, `#L758-L776`).

### 1e. Confinement, safety, limits, concurrency

- **No path confinement in `Sandbox`.** `resolvePath` passes absolute paths through (`FLUE/sandbox.ts#L189-L195`; `FLUE/node/local-env.ts#L241`). `cwd` scopes defaults, not access.
- **The host sandbox does not isolate.** "There is no isolation, by design … Do not use it as an isolation boundary for untrusted requests or multiple tenants" (`DOCS/guide/sandboxes.md#L115`). Its environment comes from `DEFAULT_LOCAL_ENV_ALLOWLIST` (`PATH`, `HOME`, `USER`, `LOGNAME`, `HOSTNAME`, `SHELL`, `LANG`, `LC_*`, `TZ`, `TERM`, `TMPDIR`, `TMP`, `TEMP`), layered with `local({env})`, then per-call `env`. The snapshot is taken once, and `process.env` is "intentionally never read" afterwards (`FLUE/node/local-env.ts#L154-L235`, `#L258`).
- **The virtual sandbox** "is isolated from the host", and its network is opt-in through an allowlist (`DOCS/guide/sandboxes.md#L43`).
- **Output limits.** `read` and `bash` keep 2000 lines or 50 KB (`bash` keeps the tail), `grep` keeps 100 matches of at most 500 characters each, and `glob` keeps 1000 results (`FLUE/agent.ts#L7-L11`, `#L459-L470`). The host `exec` kills the group once stdout+stderr pass `MAX_OUTPUT_BYTES = 64 MiB` and returns `exitCode: 1` with a note (`FLUE/node/local-env.ts#L17`, `#L115-L141`).
- **Timeouts the model sets.** `bash.timeout` is in seconds. On a timeout the tool returns a recoverable `124` result. On a host abort it rethrows (`FLUE/agent.ts#L296-L345`).
- **Concurrency.** A tool batch runs in parallel. `write` and `edit` take a per-sandbox, per-resolved-path promise lock (`WeakMap<Sandbox, Map<string, Promise>>`). "A `bash` command mutating the same file concurrently remains unsynchronized" (`FLUE/agent.ts#L119-L159`). Sessions run one operation at a time (`SessionBusyError`, `FLUE/errors.ts#L657-L665`). There are no footprints.

### 1f. Declaring a sandbox

```ts
export interface UseSandboxOptions { cwd?: string; }
export function useSandbox(sandbox: SandboxFactory, options: UseSandboxOptions = {}): void;
```

(`FLUE/hooks/use-sandbox.ts#L45-L90`.) It may be called at most once per render; a second call throws. It is not available in subagents. It may be conditional, and presence is re-read at turn boundaries. The factory value is cheap and rebuilt on every render, while `createSandbox` runs once at initialisation. `options.cwd` wraps the sandbox in `createCwdSandbox` and is "read once when a submission starts". Example: `useSandbox(local({ env: { GH_TOKEN: process.env.GH_TOKEN } }))`.

## 2. Rig (`rig-tools`, `rig-harness`, at `0.46.0-rc.1`)

### 2a. The abstraction: `Workspace`

"The disk and the commands the tools act on … Synchronous, as the rest of rig-tools: the tools call it inside [`blocking`]" (`TOOLS/workspace.rs#L23-L36`):

```rust
pub trait Workspace: Send + Sync + fmt::Debug + 'static {
    fn host(&self) -> Option<&str> { None }                       // e.g. "ssh://dev@box"; None = this machine
    fn stat(&self, path: &Path) -> io::Result<Stat>;
    fn read(&self, path: &Path, limit: u64) -> io::Result<Contents>;
    fn read_files(&self, paths: &[PathBuf], limit: u64) -> Vec<Option<Vec<u8>>> { .. }  // batch, for search
    fn excerpt(&self, path: &Path, first: usize, count: usize) -> io::Result<Excerpt> { .. }
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;  // atomic, creates dir, keeps perms
    fn replace(&self, path: &Path, bytes: &[u8], limit: u64) -> io::Result<io::Result<Contents>> { .. }
    fn write_if(&self, path: &Path, bytes: &[u8], held: &ContentHash) -> io::Result<Option<Contents>> { .. }
    fn remove(&self, path: &Path) -> io::Result<()>;               // files only
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn canonical(&self, path: &Path) -> PathBuf;                   // infallible; may answer from cache
    fn join(&self, base: &Path, path: &Path) -> PathBuf { base.join(path) }
    fn files(&self, root: &Path, glob: Option<&str>, listing: Listing) -> io::Result<Vec<PathBuf>>;
    fn spawn(&self, launcher: &Launcher, dir: Option<&Path>, program: &OsStr,
             args: &[OsString], how: Spawn) -> io::Result<Process>;
    fn watch(self: Arc<Self>, root: &Path, recursive: bool, listing: Listing) -> io::Result<Watch> { .. }
}
```

(`TOOLS/workspace.rs#L36-L174`.) Its value types (`#L176-L284`):

- `enum Stat { File { size: u64, read_only: bool, modified: Option<SystemTime> }, Dir, Other, Missing }`. A missing path is `Ok(Missing)`, not an error.
- `struct Contents { bytes: Vec<u8>, size: u64 }`. Reads are capped by `limit`, and the whole size is still reported.
- `struct Excerpt { text, total, hash: ContentHash, whole: Option<String> }`.
- `enum Listing { Visible, Hidden, All }`. Ignore-file-aware listing.

Points that matter for a design:

- **Synchronous.** Errors are `std::io::Error`. Tools convert them to `ToolExecutionError` (`not_found`, `invalid_args`, `permission_denied`, …; `text_error`, `#L374-L385`).
- **Round-trip-aware defaults.** `read_files`, `excerpt`, `replace` and `write_if` have defaults composed from `read` and `write`. Their docs say a remote workspace overrides them to "do both in one" (`#L52-L118`).
- **`write_if` is a compare-and-swap on a content hash.** `edit` uses it with what `read` saw (`Seen`) to refuse stale edits (`TOOLS/read.rs#L56-L65`; `TOOLS/edit.rs#L189`).
- **Paths are `Path`/`PathBuf`, interpreted by the workspace.** `canonical` is infallible and "may be on a thread that must not wait: a remote workspace may answer from a cache, or fold `..` only" (`#L127-L132`).
- **No env or cwd on the trait.** They live in `Launcher` (below). `dir` on `spawn` overrides the launcher's directory.

**Processes** (`TOOLS/process.rs`):

```rust
pub struct Launcher { pub prefix: Vec<OsString>, pub cwd: Option<PathBuf>,
                      pub env: Vec<(OsString, OsString)>, pub unset_env: Vec<OsString> }   // #L40-L51
pub struct Spawn { pub stdin: Option<Vec<u8>>, pub merge_stderr: bool }                  // #L207-L214
pub enum OutputLine { Out(String), Err(String) }                                         // #L220-L225
pub struct Process { child: Child, lines: Receiver<OutputLine>, group: Group, remote: Option<String> } // #L385-L391
pub enum Ended { Exited(ExitStatus), TimedOut, Stopped }                                 // #L594-L601
```

- **`Launcher` is the policy for how every process starts**: "under a wrapper program" (its examples: a process sandbox, `nice -n 10`, a container's `exec`), with a directory, variables set, and variables removed (`#L17-L22`). Every command is `detach`ed (`setsid`: a new session and process group, no controlling terminal, `GIT_TERMINAL_PROMPT=0`; `#L668-L692`).
- **Methods on `Process`.** `id`, `through(host)`, `remote()`, `lines() -> &Receiver<OutputLine>` (a std mpsc channel, fed by reader threads), `group() -> Group`, `try_wait`, `wait_for(Duration)`, `output(Option<Duration>)`, `run(deadline, sink) -> io::Result<Ended>`, `stop(grace)`, `kill()` (`#L393-L590`).
- **`Process` is a local child.** It wraps `std::process::Child`. A remote workspace returns the **local transport process** marked `through(host)`.

**Helpers.** `blocking(work)` runs work on a new OS thread named `rig-tool` and awaits a oneshot (`TOOLS/blocking.rs#L9-L22`). `Live(Arc<dyn Fn(&str) + Send + Sync>)` is a sink for streamed output lines (`TOOLS/live.rs#L24`). `Spill(pub PathBuf)` is where cut output is kept whole. It writes with `std::fs`, that is, on **this machine**, whatever the workspace (`TOOLS/spill.rs#L11-L27`). The crate doc says "Native only: the tools use the file system and processes" (`TOOLS/lib.rs#L41`).

### 2b. Backends (all first-party in `rig-tools`)

| Backend | Entry | How it implements `Workspace` |
| --- | --- | --- |
| `Local` | `pub struct Local;` (`TOOLS/workspace.rs#L413-L533`) | `std::fs` calls. `write` goes through `write_atomic` (`TOOLS/fs.rs#L82`). `rename` falls back to copy+remove across devices. `canonical` follows links of the longest existing prefix. `files` uses an ignore-aware walker that follows no symlinks. `spawn` is `launcher.spawn`. `watch` uses OS file notifications. |
| `Overlay` | `Overlay::new(inner: Arc<dyn Workspace>)`, `changes() -> Vec<FileChange>` (`TOOLS/overlay.rs#L36-L110`) | Reads go through to `inner`. Writes, removals and renames are kept in a `Mutex<Vec<(PathBuf, Option<Arc<[u8]>>)>>`. `spawn` returns `ErrorKind::Unsupported`: "an overlay runs no commands: what a command changes cannot be kept from the disk" (`#L249-L260`). Used to preview a call. |
| `Remote` | `Remote::new(name, transport: impl IntoIterator<Item = impl Into<OsString>>, launcher) -> io::Result<Self>` (`TOOLS/remote.rs#L147-L183`) | Every file operation is "one short `sh` script" run through the transport command, which takes the script as its last argument. At most `CALLS_PER_HOST = 2` run at once (a `Mutex`+`Condvar` turn). Read-only scripts retry once when the transport fails (exit 255); writes never do. `canonical` folds `..` without asking the machine. `files` runs `git ls-files` or `find`. `spawn` runs `env K=V… sh -lc 'exec "$0" "$@"'` in `cd dir`, and holds the transport's stdin open so the remote command dies when the local end goes (`#L1-L14`, `#L52-L70`, `#L606-L731`). |
| `Hosts` | `Hosts::new(inner: Arc<dyn Workspace>, remotes)` (`TOOLS/remote.rs#L720-L767`) | Routes `HOST:PATH` paths to that `Remote` and every other path to `inner`. Keeps the `HOST:` form in every path it returns, so facts and footprints round-trip. |

There is no container, microVM or hosted-provider backend in Rig. Containers and process sandboxes are reached through `Launcher.prefix` (a wrapper command) or as a `Remote` transport.

### 2c. Lifecycle and scope

- **No create or destroy.** A workspace is a value. rig-harness holds one per app: `#[derive(Resource)] pub struct ProjectWorkspace(pub Arc<dyn Workspace>)`, `Default` = `Local`. "A plugin puts another in while the app is built, such as one reached over SSH" (`HARNESS/workspace.rs#L8-L45`).
- **Scoping is per agent.** An agent's `WorkDir(PathBuf)` component, "such as a worktree of its own", is where "Its file tools resolve relative paths from it and refuse a path outside it, and its commands start in it … what a command does once started is not confined". It is saved with the agent and inherited by agents it spawns (`HARNESS/services.rs#L74-L81`). Without one, an agent uses the session's `ProjectRoot` (`#L55-L63`).
- **The process policy is app-wide.** `Processes` (an `Arc<RwLock<Launcher>>` plus secrets) is the launcher for every process. "a sandbox plugin that changes it ([`Processes::set`]) … changes how the next process starts". No command inherits `AGENT_ONLY` variables or any secret (`HARNESS/services.rs#L83-L132`).
- **Process lifetime.** `Process` drop kills and reaps its group (`TOOLS/process.rs#L603-L609`). `KillOnDrop(Group)`, held in the awaiting future, sends SIGTERM "the moment the future is dropped, such as when a call is interrupted or the app quits", then SIGKILL after `STOP_GRACE = 2s` (`#L248`, `#L638-L662`). `Shell::run` relies on this: "Dropping the future stops the command's process group at once" (`TOOLS/shell.rs#L122-L136`). The deadline is `timeout_secs`, at least 1 s.

### 2d. How tools bind to a workspace

- **Tools always exist.** Each tool is a struct with public fields, and `Default` acts on `Local` from the process cwd. Example (`TOOLS/read.rs#L26-L50`):

  ```rust
  pub struct Read { pub workspace: Arc<dyn Workspace>, pub base: Option<PathBuf>, pub confine: bool, pub seen: Seen }
  pub struct Shell { pub workspace: Arc<dyn Workspace>, pub launcher: Launcher, pub live: Option<Live>,
                     pub spill: Option<Spill>, pub cut: Option<OutputLimit>, pub confine: bool }   // TOOLS/shell.rs#L39-L57
  ```

  `Write`, `Edit`, `Search`, `List`, `Delete` and `Move` follow the `Read` shape (`TOOLS/write.rs#L25`, `TOOLS/edit.rs#L43`, `TOOLS/search.rs#L35`, …).
- **Shell.** It runs `workspace.spawn(&launcher, dir, "sh", ["-c", command], Spawn{stdin: None, merge_stderr: false})` (`TOOLS/shell.rs#L196-L213`), then `blocking(move || shell.finish(process, timeout))`.
- **Search does not shell out.** It uses `Workspace::files` + `read_files`. It is capped at `MAX_MATCHES = 200` and `MAX_FILES = 1000` (`TOOLS/search.rs#L21-L23`).
- **Per call, in rig-harness.** `app.add_tool_per_call(tool, options, make: impl Fn(CallPlace) -> T)` rebuilds the tool for every admitted call from `CallPlace { workspace: Arc<dyn Workspace>, base: PathBuf, confine: bool, live: Option<Live>, spill: Option<Spill> }`. `confine` is true exactly when the agent has a `WorkDir` (`HARNESS/per_call.rs#L43-L63`, `#L224-L240`, `#L257-L291`). A plugin can build the call on an `Overlay` to preview it. "`confine` limits the file tools' paths; it is no process sandbox" (`#L228-L229`).

### 2e. Confinement, safety, limits, footprints, concurrency

- **Path confinement.** `place` resolves the path, then refuses it with `ToolExecutionError::permission_denied("{path} is outside {root}, the only directory this tool works in")` when `confine` is set and the path is outside the canonical base (`TOOLS/workspace.rs#L286-L327`):

  ```rust
  impl dyn Workspace {
      pub fn resolve(&self, base: Option<&Path>, path: &str) -> PathBuf;
      pub fn place(&self, base: Option<&Path>, confine: bool, path: &str) -> Result<PathBuf, ToolExecutionError>;
  }
  ```

  The check is done "before any other I/O". It uses `canonical`, so symlinks are followed on `Local` but **not** on `Remote`. `Shell.confine` checks only the `cwd` argument: "it may still `cd` anywhere" (`TOOLS/shell.rs#L53-L56`).
- **Limits.** `MAX_LINES = 2000`, `MAX_BYTES = 50 KiB`, `MAX_FILE_BYTES = 16 MiB` (`TOOLS/lib.rs#L80-L85`). `OutputLimit { lines, bytes }`: the shell keeps a quarter for the first lines, a quarter for error and failure lines, and the rest for the tail (`#L87-L106`). The shell kills a command past `OUTPUT_LIMIT = 10 MiB` (`TOOLS/shell.rs#L24-L27`). The cut-off remainder can spill to a file. `read` refuses non-regular files "without being opened: opening a FIFO or a device can block forever" (`TOOLS/workspace.rs#L48-L51`).
- **Footprints.** `Touches::{Reads(Scope), Writes(Scope), Independent}` and `Scope::{Paths(Vec<PathBuf>), Anything}`, with `conflicts_with` and `normalized` (`CORE/tool/footprint.rs#L23-L166`). The file tools declare `Reads` or `Writes` of `workspace.resolve(base, path)` (`TOOLS/read.rs#L238-L242`, `TOOLS/write.rs#L93-L97`). `Shell` keeps the default, `from_annotations`, which gives `Writes(Anything)` for a tool not marked read-only (`CORE/tool/contextual.rs#L67-L74`). `normalized` turns any **relative** path into `Anything` ("A relative path names no place a guard can check", `CORE/tool/footprint.rs#L73-L86`).
- **Concurrency inside a workspace.** `Remote` allows 2 operations per host at once. `Seen` + `write_if` give optimistic concurrency for `edit`. `Overlay` is behind a `Mutex`. There is no per-path lock like Flue's.

### 2f. Declaring a workspace

There is no agent-level declaration in `rig-agent`. In rig-harness the workspace is an app resource: `app.insert_resource(ProjectWorkspace(Arc::new(…)))`. Tools are registered with `add_tool_per_call`, and scoping is a `WorkDir` component on the agent entity. Outside the harness, a caller sets the tool struct's `workspace` field directly.

## 3. Side by side

| | Flue `Sandbox` | Rig `Workspace` |
| --- | --- | --- |
| Sync or async | Async (Promises) | Sync, called from `blocking` threads |
| File content | `string \| Uint8Array`, whole file | `Vec<u8>` with a byte `limit`, plus `excerpt`, `read_files` batch, `write_if` CAS |
| Missing path | `stat` throws, `exists` never throws | `Stat::Missing` |
| Exec | `exec(cmdline, {cwd, env, timeoutMs, signal}) -> {stdout, stderr, exitCode}`, buffered | `spawn(launcher, dir, program, args, Spawn) -> Process`, streamed lines, stdin bytes |
| Env | Per call, plus the factory's base env (host allowlist) | `Launcher.env` / `unset_env`, app-wide `Processes` policy, secrets always removed |
| Cwd | `cwd` field; `createCwdSandbox` wrapper | `Launcher.cwd`, `spawn(dir)`, tool `base`, agent `WorkDir` |
| Cancellation | `signal` (prompt reject, orphan report), `timeoutMs` forwarded to the provider | Drop the future → SIGTERM, then SIGKILL 2 s later; deadline in `Process::run` |
| Remote | Provider SDK or RPC adapter; liveness polling; `SandboxDiedError` | A command-line transport; one `sh` script per op; retry reads once |
| Lifecycle | `createSandbox({id})` once per harness; no teardown | None; a value in an app resource |
| Tools without one | Built-ins absent; `harness.sandbox` throws | Always present (default `Local`) |
| Confinement | None (cwd only); host has no isolation | `confine` for file paths only; no process sandbox |
| Footprints | None; per-path write lock | `Touches` per call |
| Preview | — | `Overlay` (no exec) |
| Model-facing tools | `read, write, edit, bash, grep, glob`; replaceable by `SandboxFactory.tools` | `read, write, edit, search, list, shell, delete, move` |

## 4. Seams for a keelrs `Sandbox` (inference, not fact)

Everything in this section is reasoning from sections 1–3, not something either codebase states.

1. **Make the trait async and object-safe.** keelrs already depends on an async runtime and returns futures from tools (`keelrs/src/tool.rs`). A container, microVM or remote backend talks over a socket or RPC. Rig's synchronous trait forces a thread per call and lets `Remote` be built only from a local child process. An async trait (`Pin<Box<dyn Future + Send>>` returns, or `async fn` behind an erased wrapper) behind `Arc<dyn Sandbox>` fits every backend. A local backend can still use blocking work internally.
2. **Keep host-machine types out of the signatures.** No `std::fs::File`, `std::process::Child` or `ExitStatus`, and no `std::sync::mpsc::Receiver`. File I/O should be bytes in and bytes out (`Vec<u8>`/`Bytes`), with a byte `limit` on reads, which Rig shows is worth having. Exec should return keelrs-owned handles. Exit status should be plain data (`enum Exit { Code(i32), Signaled, TimedOut, Killed }`), so it can travel over a transport and be stored durably (see the memory note on durable agents: receipts stay plain data).
3. **Split exec into a buffered call and a streaming handle.** Flue's buffered `exec -> {stdout, stderr, exitCode}` is easy over any RPC. Rig's line streaming (`Live`, `OutputLine`) is what a UI and long commands need. A seam that serves both: `exec(ExecRequest) -> Result<ExecHandle>`, where the handle yields an async `Stream` of `Out`/`Err` chunks plus a final `Exit`, offers `kill()`, and kills on drop. A `run` helper drains it into `{stdout, stderr, exit}` with the output cap. The request should carry argv **or** a shell line, `cwd`, `env` (set and unset), optional stdin bytes, and a timeout that the backend may enforce natively (Flue's `timeoutMs` lesson).
4. **Cancellation must not depend on the backend.** Follow Flue: the keelrs wrapper races the backend future against the turn's cancellation token and returns promptly. A backend that cannot cancel reports an orphan instead of blocking the caller. Follow Rig on the host: own process group, SIGTERM, grace, SIGKILL, and kill on drop. For remote backends, a typed "sandbox died" error, distinct from abort, lets the loop tell infrastructure failure from user cancellation.
5. **Paths should be sandbox-relative strings, resolved by the sandbox.** Both systems make the backend interpret paths (Flue: POSIX strings and `resolvePath`; Rig: `canonical`/`join`, with `HOST:` paths). A keelrs `SandboxPath` (a UTF-8, POSIX-style string, absolute inside the sandbox) avoids treating a container or VM path as a host `PathBuf`. Confinement (Rig's `place`) can sit in keelrs's shared code, above the backend, using a backend `canonical` that is async and may be approximate for remote backends. Footprints should name canonical absolute sandbox paths, since relative ones degrade to `Anything` in Rig.
6. **Give lifecycle a place even if the first backend needs none.** Flue's "no teardown" leaves cleanup to the application. Rig has no lifecycle at all. A container or microVM backend has real create, start, stop and destroy. A factory-style seam (`SandboxFactory::open(instance_id) -> Arc<dyn Sandbox>`, keyed on the agent instance id as Flue does), plus an optional async `close`/`Drop` policy owned by the runtime, keeps "local, no-op" cheap and leaves room for "provision per instance, reattach after restart". Scope it per agent instance (Flue) rather than per process (Rig), because keelrs instances are durable conversations.
7. **Declare it per turn, but build it once.** This mirrors Flue: `use_sandbox(factory)` in `AgentContext` (alongside `use_tool`/`use_skill`), presence read each render, and the expensive `open` done once per instance and cached. Sandbox-backed built-in tools exist only when a sandbox is declared. The factory may replace the built-in set (Flue's `SandboxFactory.tools`), for exec-less backends.
8. **Optional capabilities, not mandatory verbs.** An overlay has no exec, and some remote backends have no watch or rename. Rather than throwing from a required method (Flue) or returning `Unsupported` (Rig's `Overlay`), the trait can expose `capabilities()` so keelrs drops the tools that need a missing capability. A typed `Unsupported` error raised before any mutation (Flue's `SandboxOperationUnsupportedError`) is still the fallback.
9. **Batch-friendly verbs where round trips cost.** Rig's defaulted `read_files`, `replace` and `write_if` (one round trip each on `Remote`) suggest keeping such defaulted methods on the trait, so `search` and `edit` stay fast on remote backends without shelling out. Flue's `grep`/`glob` shell out through `exec` instead, which ties them to backends that have a shell.
10. **Environment policy belongs to the sandbox, not to each call.** Both systems default to giving the command no secrets: Flue's host allowlist, Rig's `Processes` stripping secrets. A keelrs local backend would start from a small allowlist with explicit opt-in. Per-call env would layer on top. Values would be redacted in events.
11. **Concurrency.** The model can issue parallel tool calls, so the sandbox must be `Send + Sync`, and backends must tolerate concurrent calls. A remote backend may need its own limit, like Rig's `CALLS_PER_HOST`. Either Flue's per-path write lock or Rig's hash CAS (`write_if`) keeps parallel `edit`s safe. The hash CAS also works across processes and machines.
