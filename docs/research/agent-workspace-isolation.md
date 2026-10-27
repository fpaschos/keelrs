# How agent harnesses keep parallel agents from conflicting on code

Research for one question: when many agent instances work on the same code at once, how should keelrs keep them apart, given that it must run on a developer laptop, in Kubernetes pods, and in microVMs? Two candidate paths are on the table:

- **A. Git in the framework.** keelrs ships programmatic git tools and worktree integration. Each agent instance gets its own worktree and branch, and the runtime creates, cleans up and merges them.
- **B. Git-agnostic framework.** Isolation comes from the sandbox (one pod, microVM, container or directory per instance). Git work reaches the model as skills or tools, in the way Claude Code exposes `EnterWorktree`/`ExitWorktree` and an `isolation: worktree` subagent option.

Sections 1 to 6 are facts with citations. **Section 7 is inference**: A versus B across local, pod and microVM, and where the line between "sandbox" and "git" could sit. It flags open decisions and does not make them.

This builds on [`sandboxes.md`](https://github.com/fpaschos/keelrs/blob/research/sandboxes/docs/research/sandboxes.md) (branch `research/sandboxes`) and does not repeat it. That note covers Flue's `Sandbox`/`SandboxFactory` and Rig's `Workspace`, and proposes the seams of a keelrs `Sandbox` trait (async, plain-data exec results, a factory keyed on the agent instance id, optional capabilities). None of the other `research/*` branches touch git or worktrees. The only related line is in `tool-call-ordering.md`, which is about Rig, not workspaces.

Unlike `sandboxes.md`, this note names third-party harnesses and sandbox products, because the question is how they work.

**Sources.** Everything was read on 2026-10-27.

- Flue: local checkout `/Users/fpaschos/Dev/Work/sapp/flue` at tag `v2.3.0` (commit `deee93b4`). `FLUE/` stands for `packages/runtime/src/`, `DOCS/` for `apps/docs/src/content/docs/`, and `BP/` for `blueprints/`.
- Rig: local checkout `/Users/fpaschos/Dev/Work/sapp/rig` (as in `sandboxes.md`).
- Claude Code: official docs at `code.claude.com/docs/en/*` (fetched as Markdown).
- OpenAI Codex: official docs at `learn.chatgpt.com/docs/*`, and source at [`openai/codex@a5ef02e5`](https://github.com/openai/codex/tree/a5ef02e56147c4c3da493655fd5e2c64db13210f).
- OpenHands: official docs at `docs.openhands.dev`.
- SWE-agent at [`SWE-agent/SWE-agent@3ea751c0`](https://github.com/SWE-agent/SWE-agent/tree/3ea751c087f32b16e039a2233dd6eefecef325d5), and the SWE-ReX README.
- container-use at [`dagger/container-use@2e43e625`](https://github.com/dagger/container-use/tree/2e43e625e95216b719ec9338f4034fd3a0be2734).
- Sandbox platforms: E2B, Daytona and Modal official docs, the E2B runtime README, and Devin's docs. Conductor's docs.
- git: the `git-scm.com` manuals (`git-worktree` at 2.56.0), and [`git/git@37b3475f`](https://github.com/git/git/tree/37b3475fb530c14109211faca33716a173db61ed) for source.
- Rust: [`rust-lang/git2-rs` tag `git2-0.21.0`](https://github.com/rust-lang/git2-rs/tree/git2-0.21.0) (crates.io latest is `0.21.0`), [`libgit2` `v1.9.7`](https://github.com/libgit2/libgit2/tree/v1.9.7), and [`GitoxideLabs/gitoxide@15b71a3a`](https://github.com/GitoxideLabs/gitoxide/tree/15b71a3a0bfbab5e1667d26f2259805f4fecb216) (crates.io latest `gix` is `0.90.0`).
- Kubernetes: [`kubernetes/website@ed70e311`](https://github.com/kubernetes/website/tree/ed70e31173) and [`kubernetes-sigs/agent-sandbox@cf913b0b`](https://github.com/kubernetes-sigs/agent-sandbox/tree/cf913b0b0c759f50a290bc380f09c33789a9b4ed).
- Firecracker at [`firecracker-microvm/firecracker@f0f48ffc`](https://github.com/firecracker-microvm/firecracker/tree/f0f48ffcf24748aefde00d606c0bd026039ca95d).

Not covered: Sculptor. Its docs page on containers returned 404 and no other first-party source was found, so it is left out rather than described from secondary write-ups.

## Summary

- **Flue does nothing git-specific.** Its runtime has no git code. Isolation is "one sandbox per agent instance id", and provisioning (including any clone) belongs to the application. Subagents share the parent's sandbox and are scoped only by `cwd`.
- **Claude Code does both A and B, in layers.** Locally it creates git worktrees (`.claude/worktrees/<name>`, branch `worktree-<name>`), locks them while an agent runs, cleans them up when they hold no work, and enforces that edits and commands stay inside them. The model reaches this through `EnterWorktree`/`ExitWorktree` tools and the `isolation: worktree` subagent option. `WorktreeCreate`/`WorktreeRemove` hooks replace the git logic entirely, so the "isolated working directory" contract is VCS-agnostic and git is only the default. In the cloud, each session is its own VM that clones the repo and pushes a branch: no worktrees there.
- **Everyone else picks one layer.** Local desktop tools (Codex app, Conductor) use git worktrees. Cloud and remote harnesses (Codex cloud, OpenHands, SWE-agent, Devin, the E2B, Daytona and Modal sandboxes) use one container or VM per task, with the repo cloned, copied or baked into a snapshot. Results come back as a pushed branch, a diff or patch, or a commit made on the host.
- **container-use is the instructive hybrid.** The container gets the source tree **without** `.git`. The host keeps a private bare repository with one worktree per environment, copies the container's files into that worktree, and commits there. It needed process-level file locks around those git operations "to prevent git concurrency issues".
- **Worktrees are host-local by construction.** A linked worktree's `.git` file points at `$GIT_DIR/worktrees/<id>` by **absolute** path by default. Relative links exist only since git 2.48, and they set a repository extension that older git refuses. All worktrees share refs, objects and config, and a branch can be checked out in only one worktree. git itself still calls multiple checkouts "experimental".
- **Rust.** `git2` (libgit2) can add, list, lock and prune worktrees. `gix` can create linked worktrees but cannot remove, move or repair them, and has no push, checkout/reset or merge/rebase workflows. Every harness surveyed shells out to the `git` binary. Neither library can reach a repository that lives inside a microVM.
- **Pods and microVMs.** Kubernetes removed the `gitRepo` volume and recommends an init container that clones into an `emptyDir`. `image` volumes are stable since 1.36. Firecracker exposes only block, net and vsock devices, so a host directory (and therefore a host worktree) cannot be shared into the guest. The repo must be in a disk image or snapshot, or arrive over the network.

## 1. Flue

- **No git code in the runtime.** `grep -rniw git FLUE` and `grep -rni worktree FLUE` both return nothing. Git appears only in docs and blueprints, as one more binary on `$PATH`: "Anything on `$PATH` (`git`, `npm`, `gh`, `docker`) is reachable from the bash tool" (`DOCS/ecosystem/deploy/node.md#L237`).
- **Isolation is the sandbox.** When "each user gets their own Linux environment with git, Node.js, Python, etc. — you want a remote sandbox" (`DOCS/ecosystem/deploy/node.md#L250`). In CI, "Use `local()` only when the runner itself provides the isolation boundary" (`DOCS/ecosystem/deploy/github-actions.md#L42`).
- **Getting the repo in is the application's job.** "Application-specific data loading (git clones, R2 hydration) belongs outside this adapter" (`BP/sandbox--cloudflare-computer.md#L421-L423`). Where a provider has a git client, it is reached through the adapter's **native surface** (`workspace.git` for clones and commits, `DOCS/ecosystem/sandboxes/cloudflare-computer.md#L83`), not through the generic `Sandbox`.
- **Fan-out is a provider feature.** The microVM blueprint notes that forking is "significantly faster than `create()`", and suggests forking a base VM per short-lived agent (`BP/sandbox--boxd.md#L21-L24`, `#L490-L493`).
- **Parallel subagents share one environment.** `useSandbox()` throws in a subagent, because "Delegates share the parent agent's environment (scope work with the task call's cwd instead)" (`FLUE/hooks/use-sandbox.ts#L52-L56`; see `sandboxes.md` §1c). Flue has no per-subagent checkout.

So Flue is path B with no git tools at all: one sandbox per instance id, created by an application-supplied factory, and no teardown (`sandboxes.md` §1c).

Rig, for comparison, has no git integration either. Its only mention is a doc comment on the agent's `WorkDir`: "the directory it works in, such as a worktree of its own" (`rig/crates/rig-harness/src/services.rs#L73`). Its `Remote` workspace runs `git ls-files` to list files (`sandboxes.md` §2b).

## 2. Claude Code

### 2a. Local: git worktrees managed by the harness

From [Run parallel sessions with worktrees](https://code.claude.com/docs/en/worktrees):

- **Where and what.** `claude --worktree <name>` creates `.claude/worktrees/<name>/` at the repository root, on a new branch `worktree-<name>` ([Start Claude in a worktree](https://code.claude.com/docs/en/worktrees#start-claude-in-a-worktree)). The base is the remote default branch (`worktree.baseRef: "fresh"`, which fetches `origin/HEAD` if it is more than 24 h old, capped at 5 s), or the local `HEAD` (`"head"`). A branch name is not accepted ([Choose the base branch](https://code.claude.com/docs/en/worktrees#choose-the-base-branch)).
- **Model-facing tools.** `EnterWorktree` "Creates an isolated git worktree and switches into it", or switches into an existing one by `path`. A path outside `.claude/worktrees/` needs approval. `ExitWorktree` returns to the original directory ([Tools reference](https://code.claude.com/docs/en/tools-reference)).
- **Subagents.** `isolation: worktree` in a subagent's frontmatter gives it "a temporary git worktree … The worktree is automatically cleaned up if the subagent makes no changes" ([Subagents, supported frontmatter fields](https://code.claude.com/docs/en/sub-agents#supported-frontmatter-fields)). A forked subagent can be given `isolation: "worktree"` on the call ([Fork the current conversation](https://code.claude.com/docs/en/sub-agents#fork-the-current-conversation)).
- **Lifecycle and locking.** "While an agent is running, Claude Code holds a `git worktree lock` on its worktree so that concurrent cleanup can't remove it". A periodic sweep removes subagent and background worktrees older than `cleanupPeriodDays`, but keeps any with changed or untracked files or unpushed commits. Claude Code "writes a marker into the git metadata of every worktree it creates", and the sweep keeps any worktree without one. The sweep also releases locks left by sessions whose process has exited ([Clean up subagent and background-session worktrees](https://code.claude.com/docs/en/worktrees#clean-up-subagent-and-background-session-worktrees)). On interactive exit, a clean unnamed worktree is removed with its branch, and anything else prompts ([Clean up worktrees](https://code.claude.com/docs/en/worktrees#clean-up-worktrees)).
- **Enforcement.** Inside a worktree session, Claude Code blocks edits to paths in the main checkout, commands whose working directory resolves there, git redirects into it (`git -C`, `--git-dir`, `GIT_DIR`, `GIT_WORK_TREE`, a `cd`), and any command whose text it cannot prove keeps git inside the worktree. "None of them tracks which files a shell command writes", so `cp` or a redirect into the main checkout is not refused ([How Claude Code enforces isolation](https://code.claude.com/docs/en/worktrees#how-claude-code-enforces-isolation)). This research ran under that enforcement: compound shell commands that mentioned `git` were refused until they were split up.
- **What is shared.** "git commands in a worktree write to the main repository's shared `.git` directory, and sandboxing allows those writes" ([What worktrees share with the main checkout](https://code.claude.com/docs/en/worktrees#what-worktrees-share-with-the-main-checkout)).
- **Hardening.** Claude Code skips the repository's own filter drivers when it creates a worktree, "because a filter driver is a shell command, and anything that can write to the repository, including Claude, could have put one there". It refuses to create worktrees through symlinked paths. It refuses to adopt a directory whose git metadata resolves into the main checkout ([Troubleshooting](https://code.claude.com/docs/en/worktrees#troubleshooting)).
- **Untracked files.** A worktree is a fresh checkout, so `.env` and the like are missing. A `.worktreeinclude` file (`.gitignore` syntax) lists gitignored files to copy ([Copy gitignored files into worktrees](https://code.claude.com/docs/en/worktrees#copy-gitignored-files-into-worktrees)).

### 2b. The VCS-agnostic seam: `WorktreeCreate` / `WorktreeRemove`

From the [hooks reference](https://code.claude.com/docs/en/hooks#worktreecreate):

- `WorktreeCreate` fires for `--worktree`, `EnterWorktree`, `isolation: "worktree"`, workflow agents, background sessions and remote-control worktree mode. It "replaces that default git behavior". Input: a `name` slug. Output: "The hook must return the path to the created worktree directory", printed on stdout by a command hook, or returned as `hookSpecificOutput.worktreePath` by an HTTP hook. Any non-zero exit fails creation.
- `WorktreeRemove` receives `worktree_path` and decides removal by its exit code. Claude Code "never deletes a branch belonging to a hook-created worktree, because it only knows the path your WorktreeCreate hook returned". It also "uses git to look for changed or untracked files, so it finds none in a worktree that isn't a git checkout", and the hook must check for work itself ([WorktreeRemove](https://code.claude.com/docs/en/hooks#worktreeremove)).
- When a subagent or workflow agent finishes, Claude Code "keeps the worktree your WorktreeCreate hook made for it and doesn't run your WorktreeRemove hook".
- The docs' own example is an SVN checkout ([Non-git version control](https://code.claude.com/docs/en/worktrees#non-git-version-control)).

So the seam Claude Code exposes is narrow: **create(name) → absolute directory path**, and **remove(path)**. Everything else (dirty checks, branch cleanup, lock, sweep, resume checks) is git-specific and is lost or degraded behind a hook.

### 2c. Cloud sessions: one VM per session, clone in, push out

From [Claude Code on the web](https://code.claude.com/docs/en/claude-code-on-the-web):

- "each session runs in an isolated, Anthropic-managed VM", or on a self-hosted environment where "isolation is your deployment's responsibility" ([Security and isolation](https://code.claude.com/docs/en/claude-code-on-the-web#security-and-isolation)).
- "The cloud VM clones your current directory's GitHub remote at your current branch, not your local checkout, so push first if you have local commits." Without GitHub, the CLI uploads a git **bundle** (under 100 MB, falling back to the current branch, then to a squashed snapshot of the working tree). Clones made with `--shared`, `--reference` or `--separate-git-dir` cannot be uploaded ([Send local repositories without GitHub](https://code.claude.com/docs/en/claude-code-on-the-web#send-local-repositories-without-github)).
- Results come back as a pushed branch. `--teleport` "fetches and checks out the branch from the cloud session" locally. GitHub credentials "never enter a session's VM": a GitHub proxy attaches them server-side.

## 3. Other harnesses and sandbox platforms

| System | Unit of isolation | How the repo gets in | How results come out | git in the framework? |
| --- | --- | --- | --- | --- |
| Codex app (local) | git worktree under `$CODEX_HOME/worktrees`, detached HEAD | `git worktree add` from the selected branch, plus uncommitted changes | "Create branch here", then push or PR. Or Handoff to the local checkout | Yes (A) |
| Codex cloud | container per task (`universal` image) | "checks out your repo at the selected branch or commit SHA" | diff, then a PR | Yes, platform-side |
| Codex CLI | OS sandbox (Seatbelt, bubblewrap) around the cwd | none (works in place) | none (works in place) | Protects `.git` read-only |
| OpenHands | Docker container per runtime, or a pod | bind mounts, named volumes, optional copy-on-write overlay | REST `git/changes` and `git/diff` on the runtime | git-aware API |
| SWE-agent / SWE-ReX | a "deployment" (local, Docker, Modal, Fargate, …) | upload a local repo, or `git init` + shallow fetch | `git add -A && git diff --cached` → patch | Yes, shell commands |
| container-use | Dagger container per environment | tree from a host-side bare "fork", **without `.git`** | host copies files into a host worktree and commits; user fetches a branch | Yes, host-side |
| Conductor | git worktree per workspace | worktree on a new branch from the base | diff, PR, merge | Yes (A) |
| Devin | VM booted from a snapshot per session | repos already cloned in the snapshot | not stated on the page read | — |
| E2B | Firecracker microVM per sandbox | `commands.run("git clone …")`; the `sandbox.git` module is deprecated | "pull out the git diff, structured output, or modified files" | Removed it (B) |
| Daytona | sandbox (container or VM) | `sandbox.git.clone(...)` through the API | `sandbox.git` commit and push | Yes, in the SDK |
| Modal | container sandbox | image, or a mounted directory snapshot | filesystem or directory snapshot as an image | No |

Details and citations follow.

### 3a. OpenAI Codex

- **Cloud.** "Codex creates a container and checks out your repo at the selected branch or commit SHA", runs the setup script (with internet), then the agent (internet off by default). It "caches container state for up to 12 hours". The result is an answer and a diff of changed files, from which you can open a PR ([Cloud environments](https://learn.chatgpt.com/docs/environments/cloud-environment)).
- **Desktop app worktrees.** "Worktrees only work in projects that are part of a Git repository since they use Git worktrees under the hood." Codex creates them in `$CODEX_HOME/worktrees`, starts from the selected branch's HEAD, applies uncommitted changes, and leaves the worktree in detached HEAD "to create several worktrees without polluting your branches". It keeps the 15 most recent and "saves a snapshot of the work" before deleting one. Handoff moves a chat between a worktree and the local checkout, because "Git only allows a branch to be checked out in one place at a time" ([Worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)).
- **CLI sandbox.** It is OS-native: Seatbelt on macOS and bubblewrap on Linux. Modes are `read-only`, `workspace-write` and `danger-full-access` ([Sandbox](https://learn.chatgpt.com/docs/sandboxing)). In `workspace-write`, `.git` (with `.agents`, `.codex`, `.aws`) stays **read-only** inside the writable roots (`codex-rs/protocol/src/permissions.rs#L860-L863`). For a worktree whose `.git` is a file, Codex also resolves the `gitdir:` pointer and protects its target: "This applies to typical repos (directory .git), worktrees/submodules (file .git with gitdir pointer), and bare repos" (`#L2360-L2378`). This is the opposite default to Claude Code's sandbox, which allows writes to the shared `.git` (§2a).

### 3b. OpenHands

- **Runtime.** "When OpenHands starts, it launches a Docker container using the OH runtime image". An action execution server inside it receives actions over REST ([Runtime architecture](https://docs.openhands.dev/openhands/usage/architecture/runtime)).
- **Getting code in.** `SandboxConfig.volumes` takes bind mounts and named volumes. "Overlay mode (copy-on-write layer) is supported for bind mounts by appending ":overlay"", with per-container upper and work dirs under `SANDBOX_VOLUME_OVERLAYS` ([Volume mounts](https://docs.openhands.dev/openhands/usage/architecture/runtime#volume-mounts-named-volumes-and-overlay)). This lets many containers share one read-only checkout, each with its own writes.
- **Results.** The cloud API proxies `GET /api/git/diff` and `GET /api/git/changes` to "the conversation's runtime" ([Get Conversation Git Diff](https://docs.openhands.dev/openhands/usage/cloud/api-reference/conversations/get-conversation-git-diff)). The agent server is git-aware even though the sandbox is not.
- **Pods.** In warm-pool deployments "server pods are booted before a user is matched", stay dormant, and receive "credentials, workspace paths, session keys" through `POST /api/init` ([Deferred Init](https://docs.openhands.dev/sdk/guides/agent-server/deferred-init)). The SDK's `DockerWorkspace` is a context manager that starts the container and "Cleans up the container when done" ([Docker Sandbox](https://docs.openhands.dev/sdk/guides/agent-server/docker-sandbox)).

### 3c. SWE-agent and SWE-ReX

- SWE-ReX is "a runtime interface for interacting with sandboxed shell environments … locally or remotely in Docker containers, AWS remote machines, Modal", built to "Disentangle agent logic from infrastructure concerns" and to run agents "massively parallel" ([README](https://github.com/SWE-agent/SWE-ReX#readme)).
- SWE-agent's `Repo` protocol has `copy(deployment)` and `get_reset_commands()` (`sweagent/environment/repo.py#L20-L28`). `LocalRepoConfig` refuses a dirty repo and **uploads** the directory into the deployment (`#L98-L119`). `GithubRepoConfig` runs `git init`, `git remote add`, `git fetch --depth 1 origin <commit>` and `git checkout FETCH_HEAD` inside it (`#L163-L182`). Reset is `git fetch`, `git restore .`, `git reset --hard`, `git checkout <base>`, `git clean -fdq` (`#L31-L39`).
- The result is a patch: `git add -A && git diff --cached > /root/model.patch` (`sweagent/agent/agents.py#L856`).

### 3d. container-use

- "Each agent gets a fresh container in its own git branch", and you review it with `git checkout <branch_name>` ([README](https://github.com/dagger/container-use#readme)).
- **Host side.** It keeps a private bare repository per project under `~/.config/container-use/repos` and worktrees under `…/worktrees` (`repository/repository.go#L55-L72`, `ensureFork` `#L128-L160`). It adds that bare repo to the user's repository as a remote named `container-use` (`#L208-L226`). Creating an environment pushes the base commit to the fork as branch `<id>`, then runs `git worktree add` **in the fork, not in the user's repository** (`repository/git.go#L160-L190`).
- **Container side.** The container's source is the fork's tree at that commit, with `DiscardGitDir: true` (`repository/repository.go#L270-L277`). The agent never sees `.git`.
- **Results.** After a change, `exportEnvironment` writes the container's workdir over the host worktree, plus a `.git` file `gitdir: <fork>/worktrees/<id>`. `propagateToGit` then commits there, records state in git notes, and the user's repository fetches the branch (`repository/git.go#L265-L300`, `#L376-L398`).
- **Locking.** `RepositoryLockManager` takes file locks per operation type (`user-repo`, `fork-repo`, `notes`) "to prevent git concurrency issues when multiple container-use instances operate on the same repository simultaneously". The initial commit is locked "to prevent concurrent writes to .git/worktrees/*/logs/HEAD" (`repository/flock.go#L15-L32`; `repository/repository.go#L254-L259`).

### 3e. Conductor

"Conductor creates these separate checkouts with Git worktrees", each on a new branch based on the configured base branch after a fetch from `origin`. "A branch can only be checked out in one workspace at a time." Isolation "is development isolation, not a security boundary", because "Agents and commands still run on your Mac with your user permissions" ([Workspaces and branches](https://www.conductor.build/docs/concepts/workspaces-and-branches)).

### 3f. Sandbox platforms: Devin, E2B, Daytona, Modal

- **Devin.** "Your environment configuration is saved as a snapshot: a frozen, bootable image that every session starts from", which is "a Linux-based virtual machine with your repositories cloned". "Devin boots a fresh copy of the snapshot", and "Session changes don't persist back to the snapshot" ([Repo setup](https://docs.devin.ai/onboard-devin/repo-setup)).
- **E2B.** Its runtime is "Firecracker microVMs that resume from a snapshot", with "One Firecracker microVM per sandbox" ([e2b-dev/infra README](https://github.com/e2b-dev/infra#readme)). Forking snapshots a running sandbox once and boots N independent copies. The original is paused while it is captured, and its connections drop ([Sandbox forking](https://docs.e2b.dev/sandbox/fork)). **"The `sandbox.git` module is deprecated … Run git through the commands module instead"** ([Git integration](https://docs.e2b.dev/sandbox/git-integration)). For coding agents the advice is to "pull out the git diff, structured output, or modified files via the SDK" ([Coding agents](https://docs.e2b.dev/use-cases/coding-agents)).
- **Daytona** went the other way. "Git operations are available through the `git` module of a sandbox. Operations run through the Daytona API … without installing Git clients or executing shell commands inside it": clone, status, branches, commit, push and pull ([Git operations](https://www.daytona.io/docs/en/git-operations)). It also has fork ("duplicates the filesystem and memory into an independent sandbox") and FUSE volumes on object storage. On those volumes, "If two sandboxes write to the same path concurrently, the last write wins", and they "can not be used for applications that require block storage access" ([Persistence](https://www.daytona.io/docs/en/persistence); [Volumes](https://www.daytona.io/docs/en/volumes)).
- **Modal.** Filesystem snapshots turn a sandbox's root filesystem into an image. Directory snapshots capture one directory, which can be mounted into another running sandbox, for example "in combination with" a warm pool ([Sandbox snapshots](https://modal.com/docs/guide/sandbox-snapshots)).

## 4. git worktree mechanics and limits

From [`git-worktree`](https://git-scm.com/docs/git-worktree) (2.56.0) unless noted:

- **What is shared.** A linked worktree shares "everything except per-worktree files such as HEAD, index, etc." All `refs/` are shared except `refs/bisect`, `refs/worktree` and `refs/rewritten` ([REFS](https://git-scm.com/docs/git-worktree#_refs)). The repository config is shared, unless `extensions.worktreeConfig` is on ([CONFIGURATION FILE](https://git-scm.com/docs/git-worktree#_configuration_file)).
- **One branch, one worktree.** `add` "refuses to create a new worktree when `<commit-ish>` is a branch name and is already checked out by another worktree" ([`--force`](https://git-scm.com/docs/git-worktree#Documentation/git-worktree.txt---force)). Codex's detached HEAD and Conductor's per-workspace branch both work around this.
- **The link is a path.** The worktree's top-level `.git` file names `$GIT_DIR/worktrees/<id>`, and that directory's `gitdir` file names the worktree back ([DETAILS](https://git-scm.com/docs/git-worktree#_details)). Links are absolute by default. `--relative-paths` / `worktree.useRelativePaths` make them relative. The option first appears in the 2.48.0 manual and not in 2.47.0, and setting it "implies enabling the extensions.relativeWorktrees config … thus making it incompatible with older versions of Git" ([git-config `worktree.useRelativePaths`](https://git-scm.com/docs/git-config#Documentation/git-config.txt-worktreeuseRelativePaths)). A moved worktree needs `git worktree repair`.
- **Stale entries.** A worktree deleted without `git worktree remove` leaves administrative files that are pruned after `gc.worktreePruneExpire`. A worktree "on a portable device or network share which is not always mounted" should be `lock`ed so it is not pruned.
- **Maturity.** "Multiple checkout in general is still experimental, and the support for submodules is incomplete. It is NOT recommended to make multiple checkouts of a superproject" ([BUGS](https://git-scm.com/docs/git-worktree#_bugs)).
- **Concurrency.** git serialises writers with `<file>.lock` files created `O_CREAT|O_EXCL`. A second writer "notice[s] and fail[s]" rather than waits, and "lockfiles only block other writers. Readers do not block" (`lockfile.h#L4-L30`). Worktrees have private `HEAD` and `index`, but shared refs, objects and config, so parallel commits and fetches across worktrees can contend on shared ref and pack files. *(Inference from the shared-refs rule.)* container-use's locks (§3d) and Claude Code's `git worktree lock` (§2a) are two harnesses guarding against this in practice.
- **Ownership.** By default "Git will refuse to even parse a Git config of a repository owned by someone else, let alone run its hooks" unless `safe.directory` lists it ([git-config `safe.directory`](https://git-scm.com/docs/git-config#Documentation/git-config.txt-safedirectory)). This bites when a checkout is mounted into a container that runs as a different uid.

**Cost of getting a second copy** ([`git-clone`](https://git-scm.com/docs/git-clone)):

| Method | What it shares | Caveat from the manual |
| --- | --- | --- |
| `git worktree add` | everything except per-worktree files | host-local path links; one branch per worktree |
| `git clone /local/path` (`--local`) | `.git/objects` hardlinked "when possible" | "can race with concurrent modification to the source repository" |
| `--shared` / `--reference` | objects through `.git/objects/info/alternates` | "possibly dangerous": if the source drops objects, "the cloned repository will become corrupt" |
| `--reference … --dissociate` | borrows only during the clone | full local copy afterwards |
| `--filter=<spec>` (partial clone) | fetches objects lazily from a promisor remote | needs the remote reachable later. Claude Code's worktree creation can fail in a partial clone ([Troubleshooting](https://code.claude.com/docs/en/worktrees#worktree-creation-fails-in-a-partial-clone)) |
| `--depth` / `--revision` | truncated or single-revision history | `--depth` "Implies `--single-branch`" |

## 5. Rust options

- **`git2` (libgit2 bindings), 0.21.0.** `Repository::worktree(name, path, Option<&WorktreeAddOptions>) -> Result<Worktree>`, `worktrees()`, `find_worktree(name)` (`src/repo.rs#L3355-L3400`). `Worktree` has `name`, `path`, `validate`, `lock(reason)`, `unlock`, `is_locked`, `prune(opts)` and `is_prunable`. `WorktreeAddOptions` has `lock`, `checkout_existing` and `reference` (`src/worktree.rs#L16-L245`). There is no `remove`: prune with `working_tree(true)` is the closest. libgit2 `v1.9.7` lists `relativeworktrees` among its built-in extensions, so it can open repositories linked with relative paths (`src/libgit2/repository.c#L1864-L1870`). It is a C dependency (`links = "git2"`).
- **`gix` (gitoxide), 0.90.0.** Under worktrees, `crate-status.md` ticks "open a repository with worktrees", "read locked state" and "create linked worktrees", and leaves unticked "move, remove, and repair linked worktrees" and "obtain 'prunable' information" (`crate-status.md#L186-L196`). At the workflow level, `clone`, `fetch`, `commit` and `status` are supported. Unticked: `checkout`/`switch`/`restore`/`reset`, `merge`/`cherry-pick`/`revert`, `rebase`, `stash`/`apply`, `push`, hooks, and partial clone (`#L40-L58`).
- **Shelling out to `git`.** This is what every harness above does: Claude Code (it checks git metadata and runs `git worktree remove`), container-use (`RunGitCommand`), SWE-agent (shell commands in the deployment), and E2B, which now recommends it over its own SDK module. Rig's `Remote` also shells out (`git ls-files`).
- **Where the code runs matters more than which library.** `git2` and `gix` operate on a filesystem the keelrs process can see. A repository inside a pod or microVM is reachable only through the sandbox's `exec` and file verbs, which means the `git` binary inside the sandbox. *(Inference.)*

## 6. Pods and microVMs

### 6a. Kubernetes

- **The `gitRepo` volume is gone.** "The last version that provided a way to use this driver was Kubernetes v1.35, and it has been deprecated since the v1.11". The replacement is "an `emptyDir` volume into an init container that clones the repo using Git, then mount the EmptyDir into the Pod's container" ([Volumes, gitRepo](https://kubernetes.io/docs/concepts/storage/volumes/#gitrepo)).
- **`image` volumes** mount an OCI image or artifact read-only. They are re-resolved when the pod is recreated ([Volumes, image](https://kubernetes.io/docs/concepts/storage/volumes/#image)). The feature gate is alpha in 1.31, beta and on by default in 1.35, and stable in 1.36 ([`ImageVolume` gate](https://github.com/kubernetes/website/blob/main/content/en/docs/reference/command-line-tools-reference/feature-gates/ImageVolume.md)). A source snapshot baked as an image is therefore a native way to give many pods the same read-only base.
- **agent-sandbox (SIG Apps).** It adds a `Sandbox` CRD for "a single, stateful pod with a stable identity and persistent storage", with "creation, scheduled deletion, pausing and resuming", plus `SandboxTemplate`, `SandboxClaim` and `SandboxWarmPool`. It "delegates low-level container isolation to secure 'Sandbox Runtimes' (like gVisor or Kata Containers)" through `RuntimeClass` ([README](https://github.com/kubernetes-sigs/agent-sandbox#readme)).

### 6b. Firecracker microVMs

- **No shared filesystem.** "Firecracker provides guests with storage and network access via emulated VirtIO Net and VirtIO Block devices", plus vsock. Block devices "are backed by files on the host" and must be pre-formatted (`docs/design.md#L65-L67`, `#L74-L77`, `#L110-L114`). No filesystem-sharing device is in that list, so a host directory cannot be bind-mounted into the guest. *(Inference from the device model.)*
- **Snapshots cover memory and VM state, not disks.** A snapshot is a memory file, a VM state file, and "zero or more disk files … **managed by the users**". Memory is mapped `MAP_PRIVATE` and copied on write (`docs/snapshotting/snapshot-support.md#L70-L86`). Resuming the same snapshot more than once is "insecure" without a mechanism to keep identifiers, random seeds and tokens unique (`#L551-L559`).
- E2B's fork (§3f) is this pattern packaged as a product: snapshot once, boot N.

### 6c. How results come back, across all of the above

| Channel | Who uses it |
| --- | --- |
| Push a branch to a shared remote | Claude Code cloud, Codex app ("Create branch here"), Daytona `git.push` |
| Diff or patch read out of the sandbox | SWE-agent (`model.patch`), E2B ("pull out the git diff"), OpenHands (`/api/git/diff`), Codex cloud (diff, then PR) |
| Files copied out, then committed by the host | container-use |
| Snapshot of the sandbox or worktree | Codex app (before deleting a worktree), Modal, E2B, Daytona |
| git bundle | Claude Code uploading a local repo to a cloud VM |

## 7. A versus B, and the sandbox/git boundary (inference, not fact)

Everything in this section is reasoning from sections 1 to 6.

### 7a. Across the three deployment targets

| | Local (laptop) | Pod | microVM (Firecracker-class) |
| --- | --- | --- | --- |
| **A: keelrs manages worktrees** | Natural fit. Cheapest copy (shared objects). What Claude Code, Codex app and Conductor do. Needs locking, sweep and dirty checks (Claude Code's list is long). Not a security boundary (Conductor says so). | Works only if every instance's worktree is on one filesystem the runtime sees, so all pods share a `.git` over an RWX volume. Absolute `gitdir` links must resolve at the same path in every pod. Shared ref and lock contention crosses pods. Nobody surveyed does this. | Does not work as-is. No shared filesystem into the guest, and the worktree's `.git` points at a host path. container-use's variant (host worktree, guest gets files without `.git`, host commits) works, but then the guest is a file-sync target and git is host-only. |
| **B: sandbox isolates, git is a tool or skill** | One directory per instance is still needed, and git worktrees are the cheap way to get one locally. So B on a laptop tends to become "A behind a hook". | Natural fit. Clone (init container, or `git` through `exec`) into the pod's own volume. Push a branch or return a diff. Codex cloud, OpenHands and Claude Code cloud do this. | Natural fit. A snapshot or image with the repo baked in, fork per instance, fetch at start, then push or diff back. Devin and E2B do this. |

What the table suggests:

- **Pure A does not cross the host boundary.** Worktrees are a single-host, shared-`.git` mechanism with path-based links, so the framework would need a second mechanism for pods and VMs anyway.
- **Pure B with no git awareness loses things that harnesses consistently add.** Those are a dirty-work check before teardown, a standard way to extract results (branch, patch or diff), and branch or base selection at creation. Every harness that runs remotely added at least the extraction step (§6c). E2B removed its SDK git module and Daytona kept its own, so platforms disagree on whether that belongs in the SDK.
- **Claude Code shows a layered middle.** The harness owns "isolated working directory" as the abstraction. git worktrees are the default implementation, a hook can replace them, and the git-only extras degrade when it does (§2b).

### 7b. Candidate boundaries between "sandbox" and "git"

These are options to compare, not recommendations.

1. **Workspace provisioning is a sandbox-factory concern; git is a tool.** Extend the `SandboxFactory::open(instance_id)` seam from `sandboxes.md` §4.6 so that the local factory can create a git worktree (or a plain copy) per instance, and the pod or VM factories clone or restore a snapshot. The model sees ordinary shell or git tools, or a skill. This is Flue's stance plus a local worktree backend, and it keeps keelrs's public API git-free.
2. **A narrow `Workspace` provider between the factory and the sandbox,** shaped like Claude Code's hook contract: `create(instance_id, base) -> SandboxPath`, `teardown(path, policy)`, and an optional `has_unsaved_work(path) -> bool` and `export(path) -> Export` (`Branch { name, remote }` / `Patch(bytes)` / `Files`). The git-worktree, clone, snapshot and none implementations live behind it. The runtime owns when teardown runs. The provider owns how, and whether work would be lost.
3. **Git as a first-class runtime service (full A).** keelrs links `git2` or shells out on the host, and owns worktrees, branches, merge and cleanup. It is strongest locally and needs a separate story for pods and VMs (§7a). `gix` cannot do this alone today (no remove, push or reset). `git2` adds a C dependency.
4. **Git as model-facing tools only.** `EnterWorktree`/`ExitWorktree`-style tools, or a git skill, layered over whatever the sandbox is. This is orthogonal to 1 to 3: it decides what the **model** can do mid-run, not what the **runtime** provisions per instance.

### 7c. Open decisions

- **Who creates the per-instance directory: runtime or application?** Flue says the application (no teardown, "never creates … provider infrastructure"). Claude Code says the harness, with a hook to override. This decides whether keelrs has a teardown and sweep at all.
- **Is "unsaved work" a keelrs concept?** Claude Code's cleanup rules (dirty files, unpushed commits, submodules) are git-specific. Without git awareness, teardown must either never delete or trust the backend.
- **What is the canonical result of a run that changed code?** A branch on a remote, a patch, a host-side commit, or a sandbox snapshot. It shapes the receipt and reply types (which must stay plain data, per the durable-agents note), and whether results survive the sandbox.
- **Where do git credentials live?** In the sandbox environment (Flue `local({env})`), or behind a proxy outside the sandbox (Claude Code cloud's GitHub proxy).
- **Should keelrs protect `.git` inside the sandbox?** Codex keeps it read-only, including a worktree's resolved `gitdir`. Claude Code's sandbox allows writes so that commits work. container-use hides it from the agent entirely.
- **Base selection and the one-branch rule.** Should instances branch from the remote default (Claude Code's `fresh`), from local `HEAD`, or run detached (Codex)? And who names branches?
- **Subagents.** Flue shares the parent's sandbox (scoped by `cwd`). Claude Code can give a subagent its own worktree. Does a keelrs subagent ever get its own workspace, and from which base: the parent's current state or the default branch?
- **Rust dependency, if any git code runs on the host:** shell out to `git` (what everyone does, and needs `git` installed), `git2` (C library, full worktree API minus remove), or `gix` (pure Rust, partial). Does code that runs inside a pod or VM go through `Sandbox::exec` regardless?
- **Fan-out cost in microVMs.** Is "snapshot once, fork N" (E2B, Daytona, Firecracker's `MAP_PRIVATE` memory) part of the sandbox seam, as a `fork` capability, or left to backends? Firecracker warns that resuming one snapshot more than once needs explicit re-seeding of anything that must be unique.
