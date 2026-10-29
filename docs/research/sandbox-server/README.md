# A sandbox server on keelrs: are we on track, and what is missing?

Researched 2026-10-29 on branch `sandbox-listing-canonical` (`ba423ed`). Question: can keelrs do what flue's
sandboxes and SPACE do, so that an axum server with keelrs as its core spawns Docker containers or microVMs
for agents? What do pods, operators and kubectl give that this would lack, which stack is better, and what
should come first?

This page is the synthesis. Each claim is cited in the detail files next to it:

| File | Scope | Sources |
|---|---|---|
| [flue.md](flue.md) | flue's sandbox contract, its 12 backends, lifecycle; gap table vs keelrs | flue source, file:line |
| [space.md](space.md) | SPACE's engine end to end, crate map, a create request traced through the code, maturity; gap table vs keelrs | SPACE source, file:line |
| [isolation.md](isolation.md) | Docker, gVisor, Kata, Firecracker, Cloud Hypervisor, libkrun, Apple VZ; Rust crates; the guest-agent pattern; macOS | official docs, crates.io |
| [orchestration.md](orchestration.md) | What Kubernetes gives, kube-rs operators, landscape (agent-sandbox, E2B, OpenHands, …), a single-node axum design | official docs, crates.io |

## Verdict: on track, one layer short

The four reports agree on these points.

- **keelrs's data plane is right.** `SandboxBackend` (stat/read/write/write_if/list/canonical/exec/close)
  already matches or beats flue's `SandboxDriver`, which has no confinement, no atomic or conditional writes and
  no streamed exec ([flue.md](flue.md#gap-table-vs-keelrs)). It is the same shape as the guest agents that
  real platforms run inside the sandbox: SPACE's `spaced`, E2B's `envd`, agent-sandbox's `sandboxd`, Kata's
  `kata-agent` ([isolation.md](isolation.md), [space.md](space.md)).
- **The agent layer is ahead of SPACE.** SPACE has no model, no loop and no tools. keelrs is ahead there, and the
  host-side agent of ADR 0005 is the same split flue uses ([space.md](space.md#1-summary)).
- **What is missing is a control plane.** That is a component that owns sandbox *lifecycle* apart from any one
  agent run: create, ready, TTL and idle reaping, warm pool, limits, egress, auth, events. In keelrs today `close`
  is the only lifecycle verb. E2B's rule is "control plane and data plane never mix", and SPACE is built the same
  way.
- **The backends are missing too.** Only `local()` exists.

## Layer map: keelrs today, what to build, where to look

| Layer | keelrs today | Build | Reference to copy |
|---|---|---|---|
| Agent loop, tools | rig-driven runtime; tools via rig-tools stand-in (host disk) | Built-in sandboxed tools (#42) on `Sandbox` only | flue's six standard tools (`read`/`write`/`edit`/`bash`/`grep`/`glob`) |
| Data plane (per sandbox) | `SandboxBackend` + `local()` | `memory()`, `docker()`, later a guest-agent backend | flue `SandboxDriver`; SPACE `spaced`; agent-sandbox `sandboxd` contract |
| Sandbox key / factory | glossary term only, no API | `open(key) -> Sandbox` so agents share and recovery reattaches | flue `createSandbox({id})` |
| Liveness | `SandboxError::Died` only after `close` | Liveness probe raced in `Sandbox::race` | flue cf-sandbox polling (5 s poll, 10 s silence) |
| Lifecycle / control plane | none | `Provisioner` trait (create/inspect/list/destroy) + reconcile loop | SPACE operation dispatcher; agent-sandbox `Sandbox`/`WarmPool` |
| Desired state, durability | `STORE:` placeholders in `agent/instance.rs` | Postgres via sqlx: `sandboxes` + operations tables, idempotency keys, `SKIP LOCKED` leases | SPACE `public_operations`; orchestration.md table design |
| HTTP API | none | axum: `/v1/sandboxes` CRUD, SSE events, exec over SSE/WS, files mirroring `SandboxBackend` | orchestration.md API table; SPACE's split of management and streaming paths |
| Isolation | host process | Docker hardened + `runsc`; later Firecracker + jailer | isolation.md; SPACE launch recipe |
| Network / secrets | none (ADR 0005 defers a proxy) | `--network none` default; egress proxy with allow-list and host-side credential injection | SPACE net-gateway; Docker Sandboxes proxy |
| Auth, quotas, observability | none | API keys + owner scoping; limits; `tracing` + metrics | SPACE; kube-rs controller-rs metrics |

## Pods, operators, kubectl: what you would be rebuilding

([orchestration.md](orchestration.md#what-kubernetes-gives-you-for-free-and-what-a-small-axum-control-plane-needs))

- **Copy the pattern, not the platform.** The one Kubernetes idea to take is desired-state reconciliation. API
  handlers write intent to Postgres; a single tokio loop diffs that against the runtime (Docker containers listed
  by label) and acts idempotently. Delete works like a finalizer, and orphans are swept on startup.
- **Build small:** status conditions, resource limits, egress deny, an event stream (Docker events + SSE), TTL and
  idle reaping.
- **Skip on one node:** scheduling and bin-packing (use admission control instead), RBAC (owner column + API keys),
  declarative apply, HA of the control plane (Postgres is your etcd), and kubectl (curl, then a thin CLI).
- **Don't write an operator.** If multi-node arrives, use `kubernetes-sigs/agent-sandbox` (Apache-2.0, v1.0.8).
  It already ships `Sandbox`, `SandboxTemplate`, `SandboxClaim`, `SandboxWarmPool` and the `sandboxd` contract;
  drive it with `kube` 4.2 as a client. A custom CRD + controller would cost an estimated 600–1,000 lines and
  duplicate it. The other multi-node path is Nomad + Firecracker, which is what E2B runs; note Nomad is BUSL-1.1.

## Docker or microVMs?

([isolation.md](isolation.md))

| Option | Boundary | Host needs | On your Mac | Rust control |
|---|---|---|---|---|
| Docker (runc) | Shared kernel; "may provide incomplete isolation" | Docker | Yes (Desktop) | `bollard` 0.21.1 |
| gVisor (runsc) | User-space kernel, no KVM needed | Linux | No | via Docker runtime flag |
| Firecracker | KVM microVM, ≤125 ms boot, ≤5 MiB overhead, snapshots | Linux + KVM (bare metal or nested) | Only in Lima with nested virt (M3+, macOS 15+) | `fctools` 0.6 (semi-stable), no official SDK |
| Kata / Cloud Hypervisor | VM behind containerd shim / REST | Linux + KVM | No | `containerd-client` 0.9, community CH client |
| libkrun / microsandbox | KVM or Hypervisor.framework microVM | Linux or macOS | Yes | `libkrun` 1.19.7, `microsandbox` 0.7.11 (beta) |

Docker is not a boundary for hostile, model-written code, and access to the Docker daemon is effectively root.
So the plan is staged: Docker for development, `runsc` on Linux servers, then a microVM tier. Every VM backend
needs a **guest agent** (exec as a server stream, file RPCs over vsock). Its operations map one-to-one onto
`SandboxBackend`. Build it once as a small Rust binary, prove it inside Docker, then reuse it in Firecracker.

## SPACE: reference, remote backend, not a dependency

([space.md](space.md))

- **What SPACE is:** a full Firecracker platform with the shape keelrs's server would grow into: axum + Postgres
  operations with leases, a substrate adapter (Kubernetes / Direct), a gRPC node chain, the `spaced` guest over
  vsock, and a per-sandbox egress gateway.
- **Why not embed it:**
  - It is about 1.12M lines across about 20 services, and its Rust crate APIs carry no stability promise.
  - It needs Linux x86_64 + KVM + btrfs, so it cannot run on macOS, and it has no Docker runtime.
  - Its Direct (no-Kubernetes) mode is second-class.
- **What to borrow:**
  - the operation table with idempotency keys and leases (a durable form of keelrs's `Receipt`);
  - the create/stop/pause/suspend/wake vocabulary;
  - the capabilities struct on adapters;
  - the `spaced` protocol;
  - the jailer launch recipe;
  - the credential-injection egress proxy.
- **Cheapest route to real microVMs:** a remote keelrs backend over SPACE's HTTP file and command API.

## Recommended stack

axum 0.8 + tokio, sqlx 0.9 on Postgres, bollard 0.21 for Docker, gVisor `runsc` for untrusted code on Linux, the
keelrs `SandboxBackend` as the data plane, and a Rust guest agent over vsock (`tokio-vsock` 0.7) when VMs arrive.
Firecracker comes after that, through `fctools` or a thin client plus the jailer. Kubernetes only appears via
agent-sandbox, and only when multi-node is real.

## What to focus on, in order

1. **Sandboxed built-in tools (#42)**, written only against `Sandbox`. Every later backend then gets the full tool
   set, and the rig-tools stand-in leaves the examples.
2. **A `memory()` backend.** It is a cheap backend-independent test bed, and it exercises `capabilities` (no exec).
3. **The Sandbox key factory seam** (`open(key) -> Sandbox`), designed before the first remote backend.
4. **A `docker()` backend on bollard**, plus the liveness probe in `Sandbox::race`. Decide early between exec + tar
   and a small in-container guest agent; the agent is reusable for VMs.
5. **The server crate**: `Provisioner` trait, Postgres `sandboxes` and operations tables, reconcile loop, TTL and
   idle reaper, orphan sweep. Test it by killing the server mid-create and checking that a restart converges.
6. **The REST API** with streaming exec and files mirroring `SandboxBackend`, plus API-key auth.
7. **Hardening**: limits (memory, cpus, pids, disk), non-root, read-only rootfs, cap-drop, `--network none`, an
   egress proxy with credential injection, and `runsc`.
8. **Warm pool and image pre-pull.**
9. **A microVM tier**: Firecracker with the guest agent, or a remote backend over SPACE / agent-sandbox. Snapshots
   come later.

## Risks

- **Container escape** with plain runc.
- **Leaked containers and volumes** after a crash. The fix is label-based sweeps and idempotent reconcile, not
  `Drop`.
- **Egress exfiltration** by a prompt-injected agent.
- **Disk exhaustion.**
- **Scope creep into "our own Kubernetes".**
- **Immature dependencies:** `fctools` is semi-stable, microsandbox is beta, the SPACE crates are unstable,
  Daytona is archived and AGPL, and Nomad is BUSL.
- **The Mac and Linux split.** gVisor and Firecracker run only on Linux, so CI needs a KVM-capable Linux runner.
