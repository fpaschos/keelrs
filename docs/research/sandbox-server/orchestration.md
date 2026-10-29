# Sandbox orchestration for keelrs: what Kubernetes gives, what to build, what to skip

Researched 2026-10-29 against primary sources (links inline and in Sources). Context read: `keelrs/docs/sandbox.md`
(the `Sandbox` handle over a `SandboxBackend` with `stat/read/write/remove/rename/list/canonical/exec/close`, `local()`
today, `docker()`/remote "later") and `keelrs/docs/idea.md` (Flue-style harness in Rust, axum + SSE front).

## Summary

- **You are on track.** keelrs already has the right *data-plane* seam: `SandboxBackend` is the same shape as E2B's
  `envd`, agent-sandbox's `sandboxd`, and OpenHands' action-execution server (process exec with streaming + file ops
  served from inside the sandbox). What is missing is a *control plane*: something that owns sandbox lifecycle
  (create, ready, TTL, reap, warm pool, limits, network) independent of any one agent run.
- **Split control plane from data plane**, as E2B states outright: "Control plane and data plane never mix"
  ([e2b-dev/infra README](https://github.com/e2b-dev/infra)). Today `close` on the handle is the only lifecycle op;
  lifecycle belongs in a separate `Provisioner`-like layer the axum server owns.
- **Do not adopt Kubernetes or write an operator now.** For a single node, Kubernetes' value (scheduling across nodes,
  self-healing, RBAC, declarative API, kubectl) is mostly unused cost. The 20% you do need (desired state, reconcile,
  TTL, limits, egress deny, events) is a few hundred lines on Postgres + Docker.
- **If/when you go to Kubernetes, do not write your own Sandbox CRD.** `kubernetes-sigs/agent-sandbox` (Apache-2.0,
  Go, v1.0.8 released 2026-10-22) already ships `Sandbox`, `SandboxTemplate`, `SandboxClaim`, `SandboxWarmPool`, a
  router, and an in-pod daemon (`sandboxd`) with a standardized process/filesystem wire contract. Drive it from Rust
  with `kube` 4.2.0 as a *client*; implement a keelrs `SandboxBackend` against `sandboxd`.
- **Isolation is the real decision**, not orchestration. Plain Docker is namespaces + seccomp; for untrusted
  model-written code use gVisor (`runsc`, drop-in Docker/K8s runtime) first, Firecracker/Kata microVMs later.
- **Focus order:** (1) Docker backend for `SandboxBackend` via an in-sandbox agent or `bollard` exec; (2) Postgres
  `sandboxes` table + reconcile loop + TTL/idle reaper; (3) minimal REST + streaming exec; (4) limits + egress
  deny + gVisor; (5) warm pool; (6) only then a second provisioner (agent-sandbox on K8s, or Nomad).

## What Kubernetes gives you for free, and what a small axum control plane needs

| Kubernetes feature | What it is (source) | Small single-node server: build / simplify / skip |
|---|---|---|
| Desired-state reconciliation | Controllers are "control loops that watch the state of your cluster, then make or request changes where needed. Each controller tries to move the current cluster state closer to the desired state." ([controller docs](https://kubernetes.io/docs/concepts/architecture/controller/)) | **Build (small).** A `sandboxes` row holds desired state (`Running`/`Deleted`, `expires_at`); a tokio loop diffs it against the runtime (Docker list by label) every N seconds and on events. Make every step idempotent. This is the single most important pattern to copy. |
| Status conditions | Ready/Suspended/Finished conditions with reasons, `observedGeneration` (agent-sandbox [`sandbox_types.go`](https://github.com/kubernetes-sigs/agent-sandbox/blob/main/api/v1beta1/sandbox_types.go)) | **Build (tiny).** `status` + `reason` columns. Copy agent-sandbox's split: desired `operatingMode` vs observed `Ready`. |
| Scheduling / bin-packing | kube-scheduler picks a node per Pod by filtering then scoring ([kube-scheduler](https://kubernetes.io/docs/concepts/scheduling-eviction/kube-scheduler/)) | **Skip** on one node. Replace with admission control: refuse create when `sum(cpu, mem)` of live sandboxes exceeds host capacity. Multi-node is where Nomad/K8s earn their keep. |
| Health checks and restarts | Pod `restartPolicy`, liveness/readiness probes ([pod lifecycle](https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/)) | **Simplify.** Sandboxes are cattle tied to a session; on crash mark `Failed`, do not restart (agent-sandbox makes this a `FailedPodPolicy`). Readiness = in-sandbox daemon answers a ping. Docker restart policies exist if needed ([Docker restart](https://docs.docker.com/engine/containers/start-containers-automatically/)). |
| Resource limits | requests/limits for CPU, memory, ephemeral storage ([manage resources](https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/)) | **Build (config only).** Docker `--memory`, `--cpus`, `--pids-limit` ([Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/)) set from a template. Disk quota is the awkward one: size a volume or use a storage driver with quotas. |
| Networking / NetworkPolicy | L3/L4 allow rules; "by default, a pod is non-isolated for egress"; needs a supporting CNI ([NetworkPolicy](https://kubernetes.io/docs/concepts/services-networking/network-policies/)) | **Build (security-critical).** Default: `--network none`; opt-in egress via a dedicated bridge + host proxy allow-list. Docker Sandboxes route all egress through a host proxy that enforces policy and injects credentials so "the real credential stays outside the sandbox" ([Docker Sandboxes architecture](https://docs.docker.com/ai/sandboxes/architecture/)), a good model for keelrs' "secrets stay on host" rule (ADR 0005). |
| Storage | PV/PVC lifecycle ([persistent volumes](https://kubernetes.io/docs/concepts/storage/persistent-volumes/)); agent-sandbox `volumeClaimTemplates` | **Simplify.** One named Docker volume per sandbox for `/workspace`, deleted with the sandbox unless `retain`. Snapshot/resume is a later feature (E2B, Modal, Daytona all have it). |
| RBAC / authn | Role/RoleBinding over API verbs ([RBAC](https://kubernetes.io/docs/reference/access-authn-authz/rbac/)) | **Simplify.** API keys or JWT in an axum middleware; every row has an `owner`; every query filters by it. No verbs matrix. |
| Watch / events | `watch` "is used to track changes to an object ... as a stream ... for the efficient detection of changes" ([API concepts](https://kubernetes.io/docs/reference/using-api/api-concepts/)) | **Build (small).** Subscribe to `docker events` ([docs](https://docs.docker.com/reference/cli/docker/system/events/)) to trigger reconcile; expose `GET /sandboxes/{id}/events` as SSE; Postgres `LISTEN/NOTIFY` ([docs](https://www.postgresql.org/docs/current/sql-notify.html)) if more than one server process. |
| Declarative API | `kubectl apply` of object configs, server computes diff ([declarative config](https://kubernetes.io/docs/tasks/manage-kubernetes-objects/declarative-config/)) | **Skip** generic apply. An imperative REST API (`POST /sandboxes`) that *writes desired state* and returns immediately gets the benefit (crash-safe, reconciled) without the machinery. |
| Finalizers / garbage collection | Cleanup must finish before deletion completes ([kube-runtime finalizer](https://docs.rs/kube-runtime/latest/kube_runtime/finalizer/fn.finalizer.html)) | **Build (tiny).** `DELETE` sets `desired = Deleted`; reconciler tears down container + volume, then deletes the row. Same idempotency rule. |
| Pod security | Pod Security Standards, runtimeClass ([PSS](https://kubernetes.io/docs/concepts/security/pod-security-standards/), [RuntimeClass](https://kubernetes.io/docs/concepts/containers/runtime-class/)) | **Build (config).** Non-root user, read-only rootfs except `/workspace`, drop all caps, default seccomp ([Docker seccomp](https://docs.docker.com/engine/security/seccomp/)), userns-remap ([docs](https://docs.docker.com/engine/security/userns-remap/)), `--runtime=runsc` for gVisor. |
| kubectl UX | get/describe/logs/exec/port-forward ([kubectl](https://kubernetes.io/docs/reference/kubectl/)) | **Skip, then a thin CLI.** `curl` + the REST API is enough at first; a 200-line CLI later (`ls`, `exec`, `logs`, `rm`). |
| Multi-node HA, rolling upgrades of the control plane | etcd, leader election, Deployments | **Skip.** Postgres is your etcd. One server process; restart is safe because state is in Postgres and reconcile is idempotent. |

## Operator pattern in Rust (kube-rs)

Current versions (crates.io API, 2026-10-29): `kube` **4.2.0** and `kube-runtime` **4.2.0** (2026-07-22),
`k8s-openapi` **0.29.0** ([crates.io API: kube](https://crates.io/api/v1/crates/kube),
[kube-runtime](https://crates.io/api/v1/crates/kube-runtime), [k8s-openapi](https://crates.io/api/v1/crates/k8s-openapi)).
kube-rs is Apache-2.0 ([repo](https://github.com/kube-rs/kube)); features `client`, `runtime` ("Common components for
building Kubernetes operators": `Controller`, `watcher`, `reflector`) and `derive` (`CustomResource`)
([docs.rs kube](https://docs.rs/kube/latest/kube/)).

API facts verified on docs.rs:
- `Controller::new(main_api: Api<K>, wc: watcher::Config)`, `.owns(api: Api<Child>, wc)` (children must carry an
  `OwnerReference` back to `K`), `.run(reconcile, error_policy, context: Arc<Ctx>)` returning a `Stream`
  ([Controller](https://docs.rs/kube-runtime/latest/kube_runtime/controller/struct.Controller.html)).
- `reconcile(obj: Arc<K>, ctx: Arc<Ctx>) -> Result<Action, E>`; `error_policy(obj: Arc<K>, &E, Arc<Ctx>) -> Action`;
  `Action::requeue(Duration)` ([kube.rs application guide](https://kube.rs/controllers/application/)).
- `finalizer(api, name, obj, |Event::Apply | Event::Cleanup| ...)`; "`Event::Cleanup` must succeed before the
  Kubernetes object deletion completes"; both handlers must be idempotent
  ([finalizer](https://docs.rs/kube-runtime/latest/kube_runtime/finalizer/fn.finalizer.html)).
- Reference operator with finalizers, events, Prometheus metrics, tracing:
  [kube-rs/controller-rs](https://github.com/kube-rs/controller-rs).

Sketch of a keelrs `Sandbox` CRD + controller (illustrative, not compiled; field names mirror agent-sandbox):

```rust
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(group = "keelrs.dev", version = "v1alpha1", kind = "Sandbox", namespaced,
       status = "SandboxStatus", shortname = "sbx")]
pub struct SandboxSpec {
    pub image: String,
    pub runtime_class: Option<String>,   // "gvisor" | "kata"
    pub cpu: String, pub memory: String,
    pub egress: Vec<String>,             // allow-list -> NetworkPolicy
    pub shutdown_time: Option<Time>,     // TTL
    pub operating_mode: Mode,            // Running | Suspended
}

async fn reconcile(sb: Arc<Sandbox>, cx: Arc<Ctx>) -> Result<Action, Error> {
    let api: Api<Sandbox> = Api::namespaced(cx.client.clone(), &sb.namespace().unwrap());
    finalizer(&api, "keelrs.dev/cleanup", sb, |ev| async move {
        match ev {
            Event::Apply(sb)   => apply(sb, &cx).await,   // SSA Pod + NetworkPolicy (+ Service); patch status; requeue at shutdown_time
            Event::Cleanup(sb) => cleanup(sb, &cx).await, // delete owned children; Ok(Action::await_change())
        }
    }).await.map_err(Error::Finalizer)
}

Controller::new(sandboxes, watcher::Config::default())
    .owns(pods, watcher::Config::default())
    .run(reconcile, error_policy, ctx)
    .for_each(|_| async {}).await;
```

Verdict: this is maybe 600-1,000 lines once status conditions, TTL, warm pools and NetworkPolicy are real, and it
duplicates agent-sandbox exactly. If you go to Kubernetes, use agent-sandbox's CRDs and use `kube` only as a
typed client (derive the CRD structs, or use `DynamicObject`) from the axum server.

## Landscape

| Project | What / architecture | Isolation | License | Fit for keelrs |
|---|---|---|---|---|
| [kubernetes-sigs/agent-sandbox](https://github.com/kubernetes-sigs/agent-sandbox) | K8s controller (Go). `Sandbox`: "a single, stateful pod with a stable identity and persistent storage", pause/resume; extensions `SandboxTemplate`, `SandboxClaim`, `SandboxWarmPool` (replicas + templateRef); optional `sandbox-router` reverse proxy; `sandboxd` in-pod daemon: gRPC `ProcessService` (streaming exec) + REST filesystem API, with a versioned wire contract any runtime can implement ([USER_GUIDE](https://github.com/kubernetes-sigs/agent-sandbox/blob/main/packages/sandboxd/USER_GUIDE.md)). Spec has `shutdownTime`/`shutdownPolicy` and `operatingMode` Running/Suspended ([types](https://github.com/kubernetes-sigs/agent-sandbox/blob/main/api/v1beta1/sandbox_types.go)). Latest release v1.0.8, 2026-10-22. | Delegates to RuntimeClass (gVisor, Kata) | Apache-2.0 | **The K8s target.** Also: `sandboxd`'s process/fs contract is a ready-made spec for a keelrs remote `SandboxBackend`. |
| Kubernetes [RuntimeClass](https://kubernetes.io/docs/concepts/containers/runtime-class/) + [gVisor](https://gvisor.dev/docs/) / [Kata](https://katacontainers.io/) | RuntimeClass selects a CRI handler per Pod, with `scheduling` and `overhead`. gVisor: userspace "application kernel", OCI runtime `runsc`, works with Docker and K8s, "reduced application compatibility and higher per-system call overhead". Kata: "lightweight virtual machines that seamlessly plug into the containers ecosystem", hypervisors QEMU, Cloud-Hypervisor, Firecracker. | gVisor: syscall-intercepting kernel; Kata: VM | Apache-2.0 ([gVisor](https://github.com/google/gvisor), [Kata](https://github.com/kata-containers/kata-containers)) | gVisor is the cheapest upgrade from plain Docker: `--runtime=runsc`. |
| [E2B infra](https://github.com/e2b-dev/infra) | Go. `api` (lifecycle, placement, auth, quotas), `orchestrator` per node drives Firecracker, `envd` in every VM ("processes, PTYs, filesystem operations, file watchers, and port forwarding over Connect RPC and REST"), `client-proxy` edge router that resumes paused sandboxes; Postgres, Redis, ClickHouse. Nomad for placement (service discovery in [`packages/shared/pkg/servicediscovery/nomad`](https://github.com/e2b-dev/infra/tree/main/packages/shared/pkg/servicediscovery/nomad), autoscaler plugins in [`nomad-nodepool-apm`](https://github.com/e2b-dev/infra/tree/main/packages/nomad-nodepool-apm)). Restores from snapshots; pause preserves "full state ... indefinitely", auto-pause on timeout ([docs](https://docs.e2b.dev/sandbox)). | Firecracker microVM per sandbox, own cgroup + netns | Apache-2.0 | The architecture to grow into. Note it is Nomad, not K8s, for the hot path. |
| [Firecracker](https://firecracker-microvm.github.io/) | KVM microVMM, "<125ms" boot, "<5 MiB overhead per VM", rate limiters, jailer; [snapshots](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md) | Hardware virt (KVM) | Apache-2.0 | Later backend; needs bare metal or nested virt, kernel/rootfs images, networking (tap) work. |
| [Daytona](https://github.com/daytonaio/daytona) | Interface / control / compute planes; OCI/Docker-compatible sandboxes "<90ms"; snapshots, SSH/VNC/PTY, MCP. **Public repo archived**: "development has moved to a private codebase". | Not stated on README | AGPL-3.0 (at tag v0.190.0) | Product reference only; AGPL + archived rules out reuse. |
| [OpenHands runtime](https://docs.openhands.dev/openhands/usage/architecture/runtime) | Docker container per session; an action-execution server inside it (bash, plugins, cwd state); backend sends actions over REST, gets observations back; layered runtime-image tags to avoid rebuilds. | Docker | MIT ([repo](https://github.com/OpenHands/OpenHands)) | Closest to keelrs today: Docker + in-container server. Copy the image-tag caching idea. |
| [Modal Sandboxes](https://modal.com/docs/guide/sandbox) | Hosted. `Sandbox.create`, `exec`, terminate (SIGKILL, 137); default lifetime 5 min up to 24 h; idle timeout (active = running command, stdin, or tunnel TCP); events Created/Scheduled/Started/Ready/Finished; filesystem [snapshots](https://modal.com/docs/guide/sandbox-snapshots). | gVisor default; VM runtime option | Proprietary SaaS | API-shape reference: copy the lifecycle events and the idle-timeout definition. |
| [microsandbox](https://github.com/superradcompany/microsandbox) | Rust, "local-first microVM runtime and library"; "No setup server. No long-running daemon."; `msb` CLI, Rust/TS/Python/Go SDKs, MCP server; libkrun credited; "still beta". | microVM (libkrun) | Apache-2.0 | Candidate *embedded* microVM backend for keelrs (Rust crate, macOS + Linux) once stable. Beta risk. |
| [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/) | `sbx` CLI; local or Docker-managed cloud; each sandbox a microVM with its own Docker daemon ("Full (hypervisor)" isolation); workspace passthrough or clone mode; all egress through a host proxy with policy + credential injection ([architecture](https://docs.docker.com/ai/sandboxes/architecture/)). | microVM | Proprietary tool; local use free | Developer-side product, not a library; the egress-proxy + credential-injection design is worth copying. |
| [HashiCorp Nomad](https://developer.hashicorp.com/nomad/docs) | Single-binary scheduler for containers, VMs, raw exec; what E2B uses. | Driver-dependent | BUSL-1.1 ([LICENSE](https://github.com/hashicorp/nomad/blob/main/LICENSE)) | The lighter multi-node path vs K8s; check BUSL terms if you would offer it as a hosted product. |

## Middle path: single-node axum control plane with an in-process reconcile loop

```
            clients (SDK / CLI / keelrs agent run)
                 │ REST + SSE/WebSocket
   ┌─────────────▼──────────────────────────────────────────────┐
   │ axum server                                                │
   │  /sandboxes  CRUD ── writes DESIRED state ──► Postgres     │
   │  /exec  /files   ── data plane, proxied ──┐   sandboxes    │
   │  /events (SSE)   ◄── status changes ──┐   │   (sqlx)       │
   │                                       │   │      ▲  │      │
   │  Reconciler (tokio task)  ◄───────────┼───┼──────┘  │      │
   │    tick + docker events + NOTIFY      │   │  read desired │
   │    diff desired vs actual, act,       │   │  write status │
   │    write status ──────────────────────┘   │               │
   │  Reaper: expires_at / idle_since          │               │
   │  WarmPool: keep N Ready per template      │               │
   │                                           │               │
   │  trait Provisioner (lifecycle)   keelrs::SandboxBackend    │
   │   create/inspect/list/destroy     (stat/read/write/exec…)  │
   └─────┬─────────────────────────────────────┬───────────────┘
         │ docker API (bollard)                │ exec / fs RPC
   ┌─────▼─────────────────────────────────────▼───────────────┐
   │ host: Docker (+ runsc)  ── later: agent-sandbox on K8s,   │
   │       Nomad + Firecracker, microsandbox                   │
   │   [sandbox: /workspace volume, limits, net=none|proxy,     │
   │    optional in-sandbox daemon (envd/sandboxd-like)]       │
   └───────────────────────────────────────────────────────────┘
```

Design points:
- **Two traits, not one.** Keep `keelrs::SandboxBackend` as the per-sandbox data plane (already designed). Add a
  server-side `Provisioner` for lifecycle: `create(spec) -> RuntimeId`, `inspect`, `list(label)`, `destroy`. Docker,
  agent-sandbox and Nomad are each one `Provisioner` + one `SandboxBackend`. keelrs' `close` becomes "tell the
  control plane", not "tear down".
- **Desired state in Postgres via sqlx** (`sqlx` 0.9.0, [docs](https://docs.rs/sqlx/latest/sqlx/)): table
  `sandboxes(id, owner, template, desired, status, reason, runtime_id, expires_at, last_active_at, generation,
  observed_generation, created_at)`. API handlers only write `desired`; only the reconciler writes `status`. This
  fits the memory note that agents become durable via sqlx.
- **Reconcile loop**: `tokio::select!` on an interval, the Docker event stream (`bollard` 0.21.1,
  [docs](https://docs.rs/bollard/latest/bollard/)), and a channel poked by handlers. Per tick: list containers by
  label `keelrs.sandbox=<id>`, load rows, then for each mismatch create / destroy / mark `Failed`; orphan containers
  without a row are destroyed. Claim rows with `SELECT ... FOR UPDATE SKIP LOCKED`
  ([Postgres SELECT](https://www.postgresql.org/docs/current/sql-select.html)) so a second server instance later is
  safe.
- **TTL and idle reaping**: `expires_at` (hard TTL, like agent-sandbox `shutdownTime` and E2B timeout) and
  `last_active_at` bumped by every exec/fs call (Modal's definition of activity). Policy `Delete | Retain`
  (agent-sandbox `shutdownPolicy`); `Pause` later with snapshots.
- **Warm pool**: per template, keep `N` rows in `status=Ready, owner=NULL`; `POST /sandboxes` claims one with
  `UPDATE ... WHERE owner IS NULL ... SKIP LOCKED RETURNING`, else creates cold. Same shape as `SandboxWarmPool` +
  `SandboxClaim`.
- **Exec streaming**: SSE for output-only exec ([axum sse](https://docs.rs/axum/latest/axum/response/sse/index.html));
  WebSocket when you need stdin, resize or kill mid-stream ([axum ws](https://docs.rs/axum/latest/axum/extract/ws/index.html)).
  Cancellation maps to keelrs' existing kill (process-group SIGTERM then SIGKILL).

Minimal API surface:

| Method | Path | Notes |
|---|---|---|
| POST | `/v1/sandboxes` | `{template, ttl_secs, idle_secs, env?, egress?}` → `201 {id, status}`; claims from warm pool if possible. Idempotency-Key header. |
| GET | `/v1/sandboxes/{id}` | desired, status, reason, expires_at. |
| GET | `/v1/sandboxes?status=` | owner-scoped list. |
| PATCH | `/v1/sandboxes/{id}` | extend TTL (E2B `setTimeout`), later `operatingMode`. |
| DELETE | `/v1/sandboxes/{id}` | sets desired=Deleted, `202`. |
| GET | `/v1/sandboxes/{id}/events` | SSE of status transitions (Created, Ready, Expired, Failed, Deleted). |
| POST | `/v1/sandboxes/{id}/exec` | `{cmd|argv, cwd, env, timeout}`; SSE stream of stdout/stderr/exit; or `GET .../exec/ws` for interactive. |
| GET/PUT/DELETE | `/v1/sandboxes/{id}/files?path=` | read (limit), atomic write, remove; `POST .../files:list`, `:stat`, `:rename`, `:write_if`, `:read_many`. Mirror `SandboxBackend` 1:1 so a remote keelrs backend is a thin HTTP client. |
| GET | `/v1/templates` | image, limits, runtime (`runc`/`runsc`), warm-pool size. Config-file driven at first. |

## Recommended stack and focus order

Stack: axum 0.8.9 + tokio; sqlx 0.9.0 on Postgres; bollard 0.21.1 for Docker; gVisor `runsc` as a Docker runtime
for untrusted code; keelrs `SandboxBackend` for the data plane; `kube` 4.2.0 only when a K8s provisioner is real;
agent-sandbox v1.x as that provisioner; Firecracker/microsandbox/Kata as later isolation tiers.

Focus order:
1. **Docker `SandboxBackend`** (the `docker()` marked "later" in sandbox.md). Decide: `bollard` exec + tar for files
   (no in-container code, more round trips), or a tiny in-container daemon speaking the `sandboxd` process/fs contract
   (one binary to maintain, reusable on K8s/Firecracker). The second matches E2B, OpenHands and agent-sandbox.
2. **Control plane core**: `sandboxes` table, `Provisioner` trait, reconcile loop, finalizer-style delete, TTL + idle
   reaper, orphan sweep on startup. Test: kill the server mid-create; restart converges.
3. **REST + streaming exec** (table above), API-key auth, owner scoping.
4. **Hardening**: limits (memory, cpus, pids, disk), non-root, read-only rootfs, cap-drop, `--network none` default,
   egress proxy with allow-list and host-side credential injection, `runsc`.
5. **Warm pool + image pre-pull + layered image tags** (OpenHands-style) for startup latency.
6. **Observability**: per-sandbox events, Prometheus metrics like controller-rs (reconcile duration, errors, live
   count), tracing spans per exec.
7. **Second provisioner** only on real demand: agent-sandbox on K8s (multi-tenant, managed clusters) or Nomad +
   Firecracker (E2B path, density + snapshots). Snapshots/pause after that.

Risks:
- **Container escape.** Plain Docker shares the host kernel; model-written code is untrusted. Mitigate with `runsc`
  early; plan for a VM tier. gVisor trades syscall-heavy performance and some compatibility
  ([gVisor](https://gvisor.dev/docs/)).
- **Leaks.** Crashed server leaves containers/volumes; the label-based orphan sweep and idempotent reconcile are the
  fix, not best-effort `Drop`.
- **Egress and secrets.** An open network lets a prompt-injected agent exfiltrate; default-deny + proxy.
- **Disk exhaustion** from workspaces and images; quotas and image GC.
- **Scope creep into "our own Kubernetes".** Stop at one node; multi-node scheduling, HA, and RBAC are where to adopt
  agent-sandbox or Nomad instead of building.
- **Dependency maturity.** microsandbox is beta; Daytona's public repo is archived and AGPL; Nomad is BUSL-1.1.
- **macOS dev vs Linux prod.** gVisor and Firecracker are Linux-only; keep `local()` and plain Docker for dev.

## Sources

- Kubernetes: [controllers](https://kubernetes.io/docs/concepts/architecture/controller/), [operator pattern](https://kubernetes.io/docs/concepts/extend-kubernetes/operator/), [kube-scheduler](https://kubernetes.io/docs/concepts/scheduling-eviction/kube-scheduler/), [pod lifecycle](https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/), [resources](https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/), [NetworkPolicy](https://kubernetes.io/docs/concepts/services-networking/network-policies/), [persistent volumes](https://kubernetes.io/docs/concepts/storage/persistent-volumes/), [RBAC](https://kubernetes.io/docs/reference/access-authn-authz/rbac/), [API concepts / watch](https://kubernetes.io/docs/reference/using-api/api-concepts/), [declarative config](https://kubernetes.io/docs/tasks/manage-kubernetes-objects/declarative-config/), [kubectl](https://kubernetes.io/docs/reference/kubectl/), [RuntimeClass](https://kubernetes.io/docs/concepts/containers/runtime-class/), [Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/)
- kube-rs: [repo](https://github.com/kube-rs/kube), [docs.rs kube](https://docs.rs/kube/latest/kube/), [CustomResource derive](https://docs.rs/kube/latest/kube/derive.CustomResource.html), [Controller](https://docs.rs/kube-runtime/latest/kube_runtime/controller/struct.Controller.html), [finalizer](https://docs.rs/kube-runtime/latest/kube_runtime/finalizer/fn.finalizer.html), [kube.rs guide](https://kube.rs/controllers/application/), [controller-rs](https://github.com/kube-rs/controller-rs), crates.io API: [kube](https://crates.io/api/v1/crates/kube), [kube-runtime](https://crates.io/api/v1/crates/kube-runtime), [k8s-openapi](https://crates.io/api/v1/crates/k8s-openapi), [bollard](https://crates.io/api/v1/crates/bollard), [axum](https://crates.io/api/v1/crates/axum), [sqlx](https://crates.io/api/v1/crates/sqlx)
- agent-sandbox: [repo](https://github.com/kubernetes-sigs/agent-sandbox), [site](https://agent-sandbox.sigs.k8s.io/), [Sandbox types](https://github.com/kubernetes-sigs/agent-sandbox/blob/main/api/v1beta1/sandbox_types.go), [WarmPool types](https://github.com/kubernetes-sigs/agent-sandbox/blob/main/extensions/api/v1beta1/sandboxwarmpool_types.go), [sandboxd guide](https://github.com/kubernetes-sigs/agent-sandbox/blob/main/packages/sandboxd/USER_GUIDE.md)
- Isolation: [gVisor](https://gvisor.dev/docs/), [Kata](https://katacontainers.io/), [Firecracker](https://firecracker-microvm.github.io/), [Firecracker snapshots](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)
- Platforms: [E2B infra](https://github.com/e2b-dev/infra), [E2B sandbox docs](https://docs.e2b.dev/sandbox), [Daytona](https://github.com/daytonaio/daytona), [OpenHands runtime](https://docs.openhands.dev/openhands/usage/architecture/runtime), [Modal Sandboxes](https://modal.com/docs/guide/sandbox), [microsandbox](https://github.com/superradcompany/microsandbox), [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/), [Docker Sandboxes architecture](https://docs.docker.com/ai/sandboxes/architecture/), [Nomad](https://developer.hashicorp.com/nomad/docs), [Nomad LICENSE](https://github.com/hashicorp/nomad/blob/main/LICENSE)
- Docker / Postgres / axum: [resource constraints](https://docs.docker.com/engine/containers/resource_constraints/), [restart policies](https://docs.docker.com/engine/containers/start-containers-automatically/), [events](https://docs.docker.com/reference/cli/docker/system/events/), [seccomp](https://docs.docker.com/engine/security/seccomp/), [userns-remap](https://docs.docker.com/engine/security/userns-remap/), [bollard](https://docs.rs/bollard/latest/bollard/), [Postgres SELECT / SKIP LOCKED](https://www.postgresql.org/docs/current/sql-select.html), [NOTIFY](https://www.postgresql.org/docs/current/sql-notify.html), [sqlx](https://docs.rs/sqlx/latest/sqlx/), [axum SSE](https://docs.rs/axum/latest/axum/response/sse/index.html), [axum WebSocket](https://docs.rs/axum/latest/axum/extract/ws/index.html)
