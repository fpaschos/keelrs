# SPACE as a reference engine for a keelrs server

Sources: `/Users/fpaschos/Dev/Work/sapp/SPACE` (HEAD `82296d7c`, 697 commits, first public commit 2026-10-05, Apache-2.0) and `/Users/fpaschos/Dev/Work/sapp/keelrs` (branch `sandbox-listing-canonical`). Every crate name below has its vendor prefix removed (the real crates are named `<prefix>-space-*`). Paths are relative to each repo root.

## 1. Summary

- **What it is.** SPACE (Sandboxed Platform for Agentic Code Execution) is a multi-tenant **sandbox platform**, not an agent framework. Applications call an HTTP API to create **Firecracker microVMs** from templates, run commands in them, stream output, and pause, suspend, wake or delete them (`README.md:7-29`). It contains no model, no agent loop and no tools.
- **Engine shape.**
  - The **control plane** is an axum API, PostgreSQL holding desired state, and a durable operation queue drained by a cluster controller.
  - A **substrate** adapter (Kubernetes or "Direct") reaches the worker nodes.
  - Each node runs a chain of gRPC services over Unix sockets: node-api → sandbox-manager → node-manager → runtime-supervisor → `jailer` + Firecracker or Cloud Hypervisor.
  - btrfs reflink gives copy-on-write rootfs, and snapshots go to object storage.
  - Inside each VM, a guest agent (`spaced`) serves process and filesystem gRPC over **vsock**.
  - A separate **data plane** (axum, SSE and WebSocket) carries command streams under session-scoped tokens.
  - A per-sandbox **net gateway** enforces egress policy, intercepts TLS and injects credentials.
- **Size and maturity.** About 1.12M lines of Rust in 170 crates (about 520k of them test code), 28 `.proto` files, a Go containerd shim (about 10.7k lines), a Next.js UI, Helm, Kind and Tilt.
  - Release 0.8.0 is the "public baseline" (`docs/versioning.md:9-14`). Only 3 `TODO`/`todo!` markers exist in `crates/`.
  - The production-grade path is **Kubernetes + Firecracker on Linux x86_64 with KVM and btrfs**.
  - The Direct (no-Kubernetes) substrate is deliberately second-class: new lifecycle operations are built for Kubernetes and Direct returns `Unsupported` (`crates/libs/api/src/controllers/substrate/AGENTS.md`).
  - gVisor is an enum variant that every runtime adapter rejects.
  - The placement state machine is half-ported.
  - Snapshot, checkpoint and rollback are "incubating/hidden".
  - The Rust crate APIs carry **no stability promise**; only the HTTP API, node-api gRPC and the guest bridge gRPC are contracts (`docs/versioning.md:24-38`).
- **For keelrs:**
  - SPACE is a good **design reference**, and possibly a **remote backend**: a `SandboxBackend` written over its HTTP file and command API.
  - It is **not** a library to embed.
  - It cannot run on macOS hosts.
  - It has no Docker or OCI-container runtime.

## 2. Architecture (ASCII)

```
                         CONTROL PLANE                                         DATA PLANE
 Application ──HTTP──► api (axum)  /v1/sandboxes, /v1/operations ...       Application
   │  202 + operation_id   │ authn: API key / session / federated            │ attach with session token
   │                       ▼                                                 ▼
   │               PostgreSQL (sqlx) ◄── desired state, public_operations   data-plane (axum: SSE / WebSocket,
   │                ▲  idempotency keys, quotas, leases                      Last-Event-ID resume, drain)
   │                │  FOR UPDATE SKIP LOCKED claims (25 ms tick)             │ gRPC
   │      cluster-controller ── public-operation dispatcher                   │
   │        + reconcilers (drain, orphan, node-sync; K8s only)                │
   │                │ SubstrateAdapter                                        │
   │       ┌────────┴───────────────┐                                         │
   │   Direct substrate        Kubernetes substrate (kube 4, reflectors)      │
   │   (gRPC to node-api)      sandbox pod ─► containerd shim (Go, ttrpc)     │
   │       │                        │                                         │
 ══╪═══════╪════════════════════════╪══════════ WORKER NODE ══════════════════╪═══════════
   │       ▼                        ▼                                         ▼
   │   node-api (tonic, TCP/TLS, bearer SPACE_INTERNAL_TOKEN) ◄───────────────┘
   │       │ UDS (SO_PEERCRED authz)
   │   sandbox-manager (sbx-mgr; SQLite via rusqlite) ── template-mgr, vol-mgr, cred-mgr, build-mgr
   │       │ UDS                                    └── snapshots ⇄ object storage (versitygw locally)
   │   node-manager: netns + tap + nftables, btrfs subvolume/reflink, dm-crypt, cgroups, admission
   │       │ UDS
   │   runtime-supervisor (root): jailer ─► Firecracker 1.15.1 | Cloud Hypervisor (gVisor: stub)
   │                                   │ virtio-net (tap) │ virtio-blk (btrfs CoW rootfs) │ vsock
   │   net-gateway (per sandbox): egress policy, DNS, TLS interception, credential injection
   │   spaced-host ◄── vsock :5002 / :5001 ──► [ microVM: spaced guest daemon (process + fs gRPC) ]
```

## 3. Crate map (170 crates; grouped by role)

| Layer | Crates (lines of `.rs`, tests included) | Role |
|---|---|---|
| Public API | `api` (215k), `api-svc`, `public-api` (4.7k, route table and OpenAPI), `api-route`, `http-server`, `accounts`, `bearer-token`, `federated-identity`, `federated-grant-store`, `feature-flags` (OpenFeature/flagd), `valkey` | axum HTTP API, auth, quotas, idempotent operations, E2B-compatible wrapper (`ecw`), substrate adapters |
| Control loop | `cluster-controller-server` (105k), `cluster-controller`, `cluster-reconciler`, `cluster-trigger-queue`, `cluster-pool` (warm pools), `placement` (machine), `placement-executor`, `auto-lifecycle`, `state-reconciliation`, `service-mutex` | Operation dispatcher, reconcilers, warm pools, leader election |
| Stores | `sandbox-store`, `snapshot-store`, `snapshot-retention`, `snapshot-storage`, `object-store`, `db-migrate`, `db-migrator`, `migration-checksums`, `sqlite`, `postgres-contention`, `sql-locator` | PostgreSQL (`db/postgres`, 42 migration files) and node SQLite |
| Internal API | `internal-api-server` (44k), `internal-api-svc` | Node-state and template-release publication, ingress activation |
| Data plane | `data-plane-server` (33k), `data-plane-svc` | Command and port streams; ECW envd-compatible endpoints |
| Node services | `node-api-server/client/sim`, `node-ctl-*`, `node-mgr` (65k), `node-mgr-client/protocol`, `node-runtime-core` (22k), `node-runtime-manager`, `node-runtime-supervisor`, `node-registration-store`, `k8s-node-id` | Per-node gRPC chain down to the VMM |
| Sandbox manager | `sbx-mgr` (160k), `sbx-mgr-store` (47k), `sbx-mgr-client/protocol/migrations`, `sbx-mgr-cli`, `sandbox-portability`, `sandbox-teardown`, `sandbox-routability`, `sandbox-quarantine-store` | Sandbox lifecycle on a node: create, start, pause, suspend, restore, snapshot, export/import |
| Templates, builds, volumes | `template-mgr` (28k), `template-publisher`, `build-mgr` (23k), `vol-mgr` (21k), `btrfs`, `content-digest`, `execution-spec`, `execution-common`, `runtime-environment-policy-store` | Templates (seed/OCI recipes), builder VMs, volumes, execution-environment identities |
| Guest agent | `spaced-guest-execution` (22k), `spaced-guest-server`, `spaced-guest-protocol`, `spaced-guest-worker/cli/svc`, `spaced-host-server/client/svc`, `spaced-common` (vsock), `guest-process` | In-VM process and filesystem service, and the host-side bridge |
| Network | `net-gateway` (28k), `net-gateway-client/protocol/svc`, `net-common`, `network-roles`, `ingress-admission` | Egress and ingress proxy, DNS, TLS interception |
| Credentials | `credmgr` (23k), `credmgr-client`, `credential-injection-core/store`, `credential-provider-*`, `ecw-credential` | Secret store, KEK/CA, credential injection into egress |
| Common, ops and observability | `common-core` (24k), `common-svc`, `common-grpc`, `common-grpc-lb`, `common-jobs`, `common-io` (reflink), `observability`, `metrics`, `metrics-emit`, `grpc-metrics`, `grpc-rate-limiter`, `uds-peer-auth`, `tokio-runtime`, `systemd-watchdog`, `archcheck` (repository rules from `ARCHITECTURE.toml`), CLIs (`admin-cli`, `sbx-mgr-cli`, `ssh-cli`, ...), mocks (`mock-llm`, ...) | Shared infrastructure |

Non-Rust parts:

- `shim/`: Go containerd runtime-v2 shim `io.containerd.space.v2` (`shim/main.go:10`). It maps a Kubernetes pod task onto the sandbox-manager socket (`shim/space.go:56,352`).
- `proto/`: gRPC contracts (build, credential, execution, net, node, sandbox, spaced, template, volume).
- `templates/firecracker|cloud-hypervisor`: kernel and seed rootfs recipes.
- `docker/`: service images.
- `infra/space/kind|cluster|...`: Kind, Helm and Packer, plus a cloud Kubernetes reference.
- `Tiltfile`: dev loop.
- `ui/`: Next.js dashboard and network-policy designer.
- `db/`: per-service SQL.
- `space_smoke/`: typed Python smoke tests.

## 4. Request trace: create a sandbox, then run a command

**Create (management plane, asynchronous)**

1. **Route.** `POST /v1/sandboxes` is declared at `crates/libs/public/api/src/routes.rs:194` and mounted at `crates/libs/api/src/routes/public/mod.rs:91` → `sandboxes::create_sandbox`.
2. **Handler.** `crates/libs/api/src/routes/public/sandboxes.rs:85-129`:
   - requires an `Idempotency-Key`;
   - resolves the owner and grant, egress profiles and the warm-pool-claim flag;
   - applies the per-org sandbox quota;
   - answers **202** with `{operation_id, sandbox_id}`.
3. **Record the intent.** `create_native_sandbox_operation`, `sandboxes.rs:195-445`, writes a `CreateSandbox` row to Postgres through `PublicOperationStore`. The row carries the request fingerprint and payload, and optionally claims a warm-pool slot in the same transaction (`:277-399`). With inline mode on, a create that won a claim runs in-process before the response (`:434-438`).
4. **Dispatch.** `crates/libs/cluster/controller/server/src/public_operation_dispatcher.rs`:
   - It ticks every 25 ms and claims rows with `FOR UPDATE ... SKIP LOCKED` (`:41-45`; the SQL is at `crates/libs/api/src/public_operations.rs:2437`).
   - 16 concurrent executions run by default, with 2 slots reserved for teardowns (`:50-58`).
   - `OperationKind::CreateSandbox` routes to `CreateSandboxOperationExecutor` (`:347`).
5. **Drive.** `crates/libs/api/src/public_operation_driver.rs` renews the claim lease (stale after 60 s), retries up to 16 times and expires an operation after a 30 min deadline (`:24-44`). Each attempt is its own trace, linked to the originating request (`:84-125`).
6. **Execute.**
   - `public_sandbox_operations.rs:341-445` builds the controller request.
   - `SandboxController::create_with_sandbox_id_inner` (`crates/libs/api/src/controllers/sandbox/queries.rs:1189`) inserts the `sandboxes` row, emits `CreateRequested`, then calls `self.substrate.create_execution(...)` (`:1541-1561`).
   - If that call fails, it deletes the row and emits `CreateFailed` (`:1564-1600`).
7. **Substrate.** `controllers/substrate/mod.rs:1214` defines `trait SubstrateAdapter`; `:2508-2515` dispatches to the `Direct` or `Kubernetes` variant.
   - **Direct:** `controllers/substrate/direct.rs:3594`:
     - picks worker nodes that already cache the template (`:3610`);
     - `try_create_on_worker_candidates` (`:3053`) sends a gRPC `CreateSandbox` to each node's node-api (`:3078-3095`);
     - falls back to pull-on-create (`:3629-3653`);
     - persists the route identity and returns an `ExecutionBinding` (`:3675-3700`).
   - **Kubernetes:** creates a sandbox pod in the `sandboxes` namespace. Its RuntimeClass invokes the Go shim, whose task `Create` (`shim/space.go:352`) calls sandbox-manager over `/space/run/sandbox-manager/sandbox-manager.sock`.
8. **node-api.** `crates/libs/node/api/server/src/worker.rs:597-625`: policy-authority checks, a disk-allocation gate, then a forward to sandbox-manager over a Unix socket (`fwd_sbx_handoff!`).
9. **sandbox-manager.**
   - `service/grpc_core.rs:183` authorizes the peer and resolves the execution environment.
   - `service/create.rs:301` (`create_with_id`) inserts the SQLite row (`:388`) and reflink-copies base artifacts (`:533,:545`).
   - Start (`grpc_core.rs:562` → `service/start.rs:1792` `start_sandbox_with_upgrade_authority`) makes three node-manager calls:
     - `Allocate`, for the network slot (netns, tap): `:2233`;
     - `PrepareSandbox`, for the btrfs CoW rootfs: `:2363`;
     - `LaunchSandbox`: `:2603`.
10. **node-manager.** `crates/libs/node/mgr/server/src/service/launch_prepare.rs:267` calls the runtime-supervisor's `LaunchRuntime` (`:31`). Its RPC surface is in `proto/node/v1/node.proto:26-130` and `proto/node/runtime/v1/runtime.proto:8-14`.
11. **runtime-supervisor (root).** `crates/libs/node/runtime/supervisor/src/service.rs:443` applies cgroup limits, then calls `launch_via_jailer` (`:700-725`).
12. **VMM launch.** `crates/libs/node/runtime/core/src/launch/os/linux.rs:497-560`:
    - the jailer sets up a chroot (`pivot_root`), mount and PID namespaces, and joins the pre-made netns;
    - Firecracker is daemonized;
    - the code waits for `firecracker.sock`.
    The boot config is `node/runtime/core/src/boot.rs:11-26` (`LinuxBoot`: vCPU, memory, `guest_cid` for vsock, tap, mac, rate limiters, balloon).
13. **Guest ready.** The spaced-host connects over vsock to guest port 5002 (`crates/libs/spaced/common/src/vsock.rs:59-60`, `crates/libs/spaced/host/server/src/run.rs:155`). The client polls `GET /v1/operations/{id}` until it succeeds and the sandbox is `running`.

**Run a command (data plane)**

14. `POST /v1/sandboxes/{id}/commands` → `crates/libs/api/src/routes/public/sessions.rs:154`:
    - checks the owner and lifecycle;
    - creates a command session with a process ID and an **attachment token**;
    - enqueues a wake if the sandbox is dormant;
    - optionally streams the events inline.
15. The client attaches to the data plane at `crates/libs/data/plane/server/src/lib.rs:639-652` (`ATTACH_SESSION`, `STREAM_SESSION_EVENTS`).
16. From there the stream passes through:
    - `native/sse.rs:5` (`session_events`);
    - `native/streams.rs:418` (`connect_process_stream`, a gRPC to node-api);
    - `node/api/server/src/worker.rs:1031` (`spaced_connect_process`);
    - sandbox-manager `service/grpc_spaced_process_snapshots.rs:189`;
    - spaced-host, then vsock;
    - the guest `ExecutionService` (`crates/libs/spaced/guest/server/src/lib.rs:1-14`), which spawns the process.

    Output is cursor-based, so a client reconnecting with `Last-Event-ID` resumes where it left off (`docs/architecture.md`, "Security boundaries").

## 5. Stack

| Concern | SPACE choice |
|---|---|
| HTTP | axum 0.8, tower/tower-http 0.5/0.7, hyper 1; SSE and WebSocket in the data plane; OpenAPI served at `/openapi-m1.json` |
| RPC | tonic/prost 0.14 everywhere (TCP with internal TLS between nodes; Unix sockets on a node), tonic-health and tonic-reflection; Go side uses containerd ttrpc |
| Control-plane database | PostgreSQL via **sqlx 0.9**; operation queue with `SKIP LOCKED` claims, lease tokens and execution generations as fences |
| Node database | SQLite via rusqlite 0.39 (bundled), each behind one store crate (enforced by archcheck) |
| Cache and rate limits | Valkey/Redis (`redis`, `deadpool-redis`) with Lua token buckets (`api/src/cluster_admission/token_bucket.lua`) |
| Orchestration | kube 4.2 + k8s-openapi 0.28 (reflector caches), or the Direct substrate |
| VMM | Firecracker 1.15.1 + jailer (default "E0" environment), Cloud Hypervisor (alternative), gVisor (stub); Linux x86_64, KVM, guest kernel 6.1.155 (`docs/execution-environments.md`) |
| Storage | btrfs subvolumes and reflink CoW, loop devices, dm-crypt, sparse-aware copy, zstd; object storage for templates and snapshots |
| Networking | netns, tap, nftables, rtnetlink, hickory-resolver; egress gateway with TLS interception (rcgen, rustls) |
| Guest channel | tokio-vsock; spaced gRPC bridge (`proto/spaced/*`) |
| Auth | bcrypt API keys, hashed session tokens, session-scoped attachment tokens, federated identity, `SPACE_INTERNAL_TOKEN` bearer between nodes, `SO_PEERCRED` on Unix sockets (`crates/libs/uds-peer-auth`) |
| Observability | tracing + tracing-opentelemetry, OTLP 0.32, `metrics`; one operation span per attempt, linked to its request |
| Flags | open-feature + flagd |
| Tooling | cargo-vet, cargo-deny, archcheck ratchets, lefthook, Tilt, Kind, mdBook |

## 6. Maturity: implemented vs stubbed

| Area | Status | Evidence |
|---|---|---|
| Native create, delete, commands, list, inspect | Supported ("M1Supported") | `crates/libs/public/api/src/routes.rs:191-230` |
| File API (stat, read, write, write_batch, list, mkdir, remove, rename, change tracking) | **Incubating** | `routes.rs:200-210` |
| Pause/resume, suspend/restore, wake, idle auto-suspend (6 h) | Implemented on Kubernetes | `docs/architecture.md`; `sandboxes.rs:78` |
| Snapshots, checkpoint, rollback | Incubating and hidden | `README.md` (SPACE API section); `routes.rs:227-228` |
| Warm pools, base snapshots | Implemented (Kubernetes; single-node base snapshots) | `docs/warm-pools.md`, `docs/base-snapshots.md` |
| Direct (no-Kubernetes) substrate | Partial by design: no forks, node pools, drain or evacuation; failed retires are never retried | `controllers/substrate/direct.rs:1-52` |
| Placement state machine | Half-ported: "operations land one per PR", `UnimplementedOperation` | `crates/libs/placement/machine/src/lib.rs:17-22` |
| gVisor runtime | Enum only; rejected by every adapter | `node/runtime/core/src/boot.rs:74`, `control.rs:93`, `build/mgr/src/vm.rs:27` |
| Docker or OCI-container runtime | **None** (Docker is used only for Kind and image builds) | — |
| Guest execution as an embeddable library | Proposal (RFC) | `docs/space/rfcs/2026-10-06-guest-execution-embedding.md` |
| Rust crate API stability | None promised | `docs/versioning.md:36-38`; `spaced/guest/execution/README.md` |
| Host platforms | Linux x86_64 with KVM, btrfs, loop devices; not macOS | `docs/installation.md` |
| Code health | 3 TODO markers; strict clippy deny-list; 1000-line file cap ratchet | `Cargo.toml` lints; `ARCHITECTURE.toml` |

## 7. Gap table: what keelrs lacks to do the same

keelrs today is about 3.3k code lines. It provides:

- `Runtime` and `AgentInstance` actors with dispatch → `Receipt` → `read` (`keelrs/src/runtime.rs`, `keelrs/src/agent/instance.rs`);
- a `Sandbox` handle over the `SandboxBackend` trait (`keelrs/src/sandbox/mod.rs:230-315`);
- one backend, `local()` (`keelrs/src/sandbox/local.rs:57`).

Agents run on the host and only tools touch the sandbox (ADR 0005). There is no server, persistence, Docker backend, auth or telemetry. The dependencies have no axum, sqlx or tracing (`keelrs/Cargo.toml`).

| Layer | SPACE | keelrs now | Gap for "keelrs + axum server that spawns containers/microVMs" |
|---|---|---|---|
| Agent layer | none | Agent, turn loop on Rig `AgentRun`, tools, skills (ADR 0004) | keelrs is ahead; SPACE has nothing here |
| HTTP API | axum, versioned routes, OpenAPI, idempotency keys, 202 + operation | none | A server crate: dispatch → receipt, read, SSE events, sandbox CRUD |
| Async operation model | Durable `public_operations` + dispatcher with claims, leases, retries, deadlines | `Receipt` + in-memory `settled` map; `STORE:` placeholders (`agent/instance.rs:125-170`, `event.rs:4-12`) | Durable events and receipts in sqlx (planned); idempotency keys; retry and deadline policy |
| Persistence | PostgreSQL (sqlx) + node SQLite + migrations | none | sqlx store for events, instances and sandbox records; migrations |
| Sandbox registry and lifecycle | sandbox rows, lifecycle states, idle auto-suspend, delete-cancels-create | `Sandbox::new`/`close` only (close defaults to no-op, `sandbox/mod.rs:311`); "Sandbox key" is in `GLOSSARY.md` but not in code | Sandbox manager keyed by Sandbox key: open-or-reuse, refcounted sharing, idle reaping, crash recovery |
| Runtime driver | Firecracker/Cloud Hypervisor via jailer, cgroups, per-node supervisor | `local()` only (tokio::process, process groups) | `docker()` backend (bollard or the Docker CLI); later a microVM backend, or a remote backend over SPACE |
| Guest agent | `spaced` over vsock: process start/connect with output cursors, PTY, file ops, watches | none needed for local | Docker: `docker exec` + tar/archive API is enough for v1; microVM: a guest agent like `spaced` |
| Data plane | Separate streaming service, attachment tokens, reconnect by cursor | `Execution` stream in-process (`sandbox/mod.rs:708-760`) | SSE for agent events; reconnect via event offsets (falls out of durable events) |
| Scheduling and multi-node | Direct candidate selection or Kubernetes, warm pools, admission | single process | Not needed for v1; keep the backend trait remote-capable (already designed: `canonical_all`, `read_files` batch round trips) |
| Storage | btrfs CoW rootfs, snapshots to object storage, volumes | host dir as work dir | Docker volumes or bind mounts for the work dir; snapshot or commit later |
| Network and egress | netns + tap + nftables; gateway with policy, TLS interception, credential injection | ADR 0005 "proxies later"; env allowlist for local commands | Container `--network none` or a proxy-only network as the first step; egress proxy later |
| Auth | API keys, session tokens, scoped attachment tokens, peer creds | none | Server bearer or API keys; per-instance scoping |
| Observability | tracing + OTLP, per-attempt spans, metrics | canonical `Event`s only | `tracing` spans around dispatch, turn and tool call; metrics |
| Resource limits | cgroups via supervisor, admission, quotas, rate limits | timeouts on exec only | Docker `--memory/--cpus/--pids-limit`; per-tenant quotas in the server |
| Platform | Linux, KVM | any (dev on macOS) | Docker works on macOS (Docker Desktop); microVMs need Linux with KVM |

## 8. What to borrow (and what SPACE is missing)

**Borrow (design, not code):**

1. **Operation records + idempotency keys + `SKIP LOCKED` claim/lease queue** (`sandboxes.rs:195-445`, `public_operation_driver.rs:24-44`, `public_operations.rs:2437`). This maps directly onto keelrs's `Receipt`, made durable with sqlx: a dispatch writes a row; a worker claims, renews and settles it. Each attempt's span links to its request.
2. **Adapter trait discipline.**
   - `SubstrateAdapter` makes every lifecycle operation a *required* method and exposes static facts through a `SubstrateCapabilities` struct, never through "which variant is this" tests (`controllers/substrate/AGENTS.md`).
   - keelrs already has `SandboxBackend` + `Capabilities`. Keep `close` and any future `pause`/`snapshot` as required methods, or keep them capability-gated.
3. **Lifecycle vocabulary.** create/delete, stop/start, pause/resume (in memory), suspend/restore (persisted), wake (any valid path), plus an idle auto-suspend that is a limit, not a maximum lifetime (`AGENTS.md`, "Sandbox lifecycle vocabulary"). It also defines delete-cancels-queued-create.
4. **Split management from streaming**, with session-scoped attachment tokens distinct from management tokens, and cursor-based reconnect.
5. **Guest protocol shape for a future microVM backend.**
   - `proto/spaced/*` (process start/connect/signal/stdin/PTY; fs stat/read/write/list/move/remove; watches), over vsock ports 5001/5002.
   - It lines up almost 1:1 with keelrs's `SandboxBackend` ops (`stat`, `read`, `write`, `remove`, `rename`, `list`, `exec`).
6. **Firecracker launch recipe.** jailer + pre-created netns + chroot + API socket wait (`launch/os/linux.rs:497-560`); `LinuxBoot` inputs (`boot.rs:11-26`); btrfs reflink rootfs.
7. **Egress gateway with credential injection** (`crates/libs/net/gateway/server`, `credential/injection/*`). This is exactly the "credential proxy" ADR 0005 defers: secrets stay outside the box and the proxy adds them to matching outbound requests.
8. **Cheapest path to microVMs: a `remote`/`space` backend.** Implement `SandboxBackend` over SPACE's HTTP API:
   - `files:stat|read|write|list|remove|rename` and `commands` with SSE;
   - `canonical` through `exec realpath -m`, with `canonical_all` batched in one exec;
   - `write_if` uses its default.

   SPACE also has an E2B-compatible surface (ECW), so a backend written against that protocol would also target other E2B-compatible services.

**What SPACE is missing for the keelrs use case:**

- No agent framework: no model, turn loop, tools, skills or conversation state. Its only LLM piece is a mock (`crates/apps/mock/llm`).
- No Docker or OCI-container runtime; gVisor is a stub. Firecracker and Cloud Hypervisor need Linux with KVM and btrfs, so there is no local dev on macOS.
- Operationally heavy: PostgreSQL, Valkey, object storage, Kubernetes (Kind) with privileged nodes, about 20 services. The no-Kubernetes Direct mode is intentionally limited.
- No stable Rust API to depend on. The guest-execution library embedding is only an RFC. Consume it over HTTP, or copy designs.
- The file API is still incubating; the half-ported placement machine shows the control plane is mid-refactor.
- Very young as a public project (released 2026-10-05; about 700 commits in 3 weeks), so expect churn before 0.9/1.0.
