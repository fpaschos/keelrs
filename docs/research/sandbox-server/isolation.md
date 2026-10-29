# Isolation runtimes for an untrusted-code agent sandbox

Researched 2026-10-29 from primary sources (project repos, official docs, docs.rs, crates.io). Every claim carries a link; numbered refs are in [Sources](#sources). Where a project publishes no figure, the table says "no official figure" rather than guessing.

## Summary

- **Docker (runc)** shares the host kernel. Docker itself says its defaults "may provide incomplete isolation", especially combined with kernel bugs [D1]. The default seccomp profile blocks only "around 44 system calls out of 300+" [D2], and user namespaces are off by default [D1]. It is the easiest to drive from Rust (`bollard` 0.21.1) and runs on a Mac through Docker Desktop's Linux VM [D5]. It is fine for development and trusted code, but weak against hostile code.
- **gVisor (runsc)** puts a user-space kernel (the Sentry) between the workload and the host: "No system call is passed through directly to the host" [G2]. It is an OCI runtime that plugs into Docker and containerd [G6]. Its default `systrap` platform needs no KVM, so it runs on ordinary cloud VMs [G1]. The cost is overhead on syscall-heavy and small-file workloads [G4]. It runs on Linux only [G5].
- **Firecracker** is the reference microVM: ≤125 ms from `InstanceStart` to guest init and ≤5 MiB of VMM overhead per VM [F1]. It has full and diff snapshots [F3], a vsock bridged to a host Unix socket [F4], and only net, block and vsock devices (no virtio-fs) [F6]. It needs Linux with KVM [F2]. It powers AWS Lambda and Fargate [F2], Fly.io [P1] and the open-source E2B runtime [E1].
- **Cloud Hypervisor** is a Rust VMM for general-purpose guests. It has a REST API on a Unix socket, snapshot/restore, virtio-fs, vsock and hotplug [C1][C2]. It needs KVM or MSHV and host kernel ≥ 6.8 [C1].
- **Kata Containers** wraps a VM (QEMU, Cloud Hypervisor, Firecracker, Dragonball, StratoVirt) behind the containerd shim v2 API, with a Rust `kata-agent` in the guest that speaks ttrpc over vsock [K1][K2]. It is the heaviest stack to operate.
- **libkrun**, with the tools built on it (krunvm, microsandbox), is a library VMM. It runs on Linux/KVM **and on macOS Apple silicon (HVF)** [L1]. Its own security model warns that the guest can reach whatever the VMM process can reach, so the VMM must be jailed [L2].
- **Apple Virtualization.framework** (and Apple's `containerization` package, one VM per container) is macOS-only. It provides vsock, virtio-fs, VM save/restore (macOS 14+) and nested virtualization (macOS 15+) [A1–A4][A5].
- **Recommendation:** put a `docker()` backend (bollard) behind `SandboxBackend` now, optionally using `runsc` as the runtime on Linux servers. Then add a Firecracker backend on Linux/KVM with a small Rust guest agent over vsock. Develop it on the Mac in a nested Lima VM (M3+, macOS 15+) or on a Linux box. Details are in [Recommendation](#recommendation).

## Comparison table

| | Docker / containerd + runc | gVisor (runsc) | Kata Containers | Firecracker | Cloud Hypervisor | libkrun / krunvm | Apple Virtualization |
|---|---|---|---|---|---|---|---|
| **Isolation boundary** | Namespaces, cgroups, capabilities, seccomp on a **shared host kernel**; "incomplete isolation" with kernel bugs [D1]; seccomp blocks ~44 of 300+ syscalls [D2] | User-space kernel (Sentry); no syscall passed straight to the host; host access limited to a Gofer, a few calls and a vNIC [G2] | Hardware VM per pod, as a "second layer of defense" [K1] | KVM microVM, plus seccomp, cgroups/namespaces and dropped privileges through the `jailer` [F2][F6] | KVM/MSHV VM with a "small attack surface" goal [C1] | KVM/HVF VM, but devices (virtio-fs, TSI) act with the VMM's host rights; jail the VMM [L2] | Hypervisor VM per container (Apple `containerization`) [A5] |
| **Cold start** | Container start, no kernel boot; no official figure | Mostly Docker's own overhead, per gVisor [G4] | VM boot plus agent; no official figure | ≤125 ms to guest init; VMM ready in ~6–60 ms [F1] | No official figure [C1] | microsandbox claims <100 ms guest boot on M1 [L4] | "sub-second" (`containerization`) [A5] |
| **Density / memory** | Shared kernel; no per-sandbox kernel | Small, "mostly fixed" Sentry overhead per sandbox [G4] | Guest kernel and agent per pod; no official figure | ≤5 MiB VMM overhead (1 vCPU, 128 MiB) [F1] | No official figure; memory resize and hotplug [C1] | No official figure | No official figure |
| **Snapshot / restore** | `docker checkpoint` is **experimental** and needs CRIU [D3] | `runsc checkpoint/restore`; restore host needs the same CPU features [G3] | Through the hypervisor; not a Kata API | Full and diff snapshots, lazy page loading; restoring one snapshot twice is insecure (shared IDs/randomness); same host and CPU only [F3] | `/vm.snapshot` and `/vm.restore`; not stable across versions [C1][C2] | Not documented | `saveMachineStateTo` / `restoreMachineStateFrom`, macOS 14+ [A3] |
| **Networking** | Bridge or `none` drivers [D4] | netstack (user-space TCP/IP); `--network=none`; host mode weakens isolation [G7] | CNI through the shim | Host TAP device per VM plus NAT rules, which need root [F5] | virtio-net (TAP) | TSI (guest sockets through the VMM over vsock) or virtio-net to passt/gvproxy [L1] | Virtio NAT or bridged (framework) |
| **Rootfs / OCI** | Native OCI images | OCI runtime under Docker or containerd [G6] | OCI bundle shared over virtio-fs; with Firecracker a block device (devmapper) is required [K1][K3] | **Block devices only** (ext4 image files); no OCI, no virtio-fs [F6][F7] | Block, virtio-fs, pmem [C1][C3] | OCI images (krunvm through buildah; microsandbox from registries) [L3][L4] | OCI image APIs and ext4 rootfs (`containerization`) [A5] |
| **Host requirements** | Linux kernel; on macOS, Docker Desktop's VM [D5] | Linux; `systrap` needs no virtualization, `kvm` platform best on bare metal [G1] | KVM (nested on cloud VMs) | Linux plus KVM; tested on AWS metal, host kernels 5.10 / 6.1 / 6.18 [F2] | KVM or MSHV, host kernel ≥ 6.8 [C1] | Linux KVM, or macOS HVF on ARM64 [L1] | macOS; `containerization` needs Apple silicon and macOS 26 [A5] |
| **Runs locally on a Mac?** | Yes (inside Docker Desktop's VM) | Only inside a Linux VM | Only inside a Linux VM with nested virtualization | Only inside a Linux VM with nested virtualization (M3+, macOS 15+) [M1] | Same as Firecracker | **Yes, natively** | **Yes, natively** |
| **Published production users** | Ubiquitous | Modal Sandboxes (gVisor option) [P2] | Integrates with CRI-O and containerd [K1] | AWS Lambda and Fargate [F2]; Fly.io [P1]; E2B [E1] | Hypervisor option in Kata [K2] | Podman `--runtime=krun` via crun, muvm, krunkit [L1] | Apple `container` CLI [A6] |

**Cloud nested virtualization** (only relevant to the KVM-based options):

- **GCE** supports nested virtualization with a KVM L1 hypervisor. It excludes E2, Arm and most AMD machine types, and costs "10% or greater" on CPU-bound work [N1].
- **EC2** supports it on non-metal M7i/C7i/R7i/M8i/C8i/R8i/X8i/I7i(e) families, enabled with `--cpu-options NestedVirtualization=enabled`. AWS points latency-sensitive users to bare metal [N2].
- **gVisor `systrap`** needs no nesting at all [G1].

## Rust crates (versions verified on crates.io, 2026-10-29)

| Crate | Version (updated) | What it gives you | Source |
|---|---|---|---|
| `bollard` | 0.21.1 (2026-08-16) | Async (tokio/hyper) Docker and Podman API, targets API 1.52 with version negotiation [R1]. Has `create_exec`/`start_exec` (streaming `StartExecResults`), `upload_to_container[_streaming]`, `download_from_container`, `create_checkpoint`, `stats` [R2] | https://crates.io/crates/bollard |
| `containerd-client` | 0.9.0 (2026-04-15) | gRPC bindings to the containerd API [R3] | https://crates.io/crates/containerd-client |
| `containerd-shim`, `containerd-shim-protos` | 0.12.0 (2026-10-28) | Runtime v2 shim wrapper and ttrpc shim protos [R3] | https://crates.io/crates/containerd-shim |
| `ttrpc` | 0.10.0 (2026-10-09) | ttrpc (the transport kata-agent uses) | https://crates.io/crates/ttrpc |
| `fctools` | 0.6.0 (2026-05-11) | Firecracker host SDK: `vmm`, `vm` (api, snapshot, shutdown), `process_spawner`, `extension::{http_vsock, grpc_vsock, link_local, snapshot_editor, metrics}`. Describes itself as "semi-stable"; 0.6 targets FC ≥ 1.7 on x86_64, 0.7 targets FC ≥ 1.14 on x86_64/aarch64 [R4][R5] | https://crates.io/crates/fctools |
| `fcnet` | 0.13.0 (2026-05-11) | Firecracker TAP/netns networking setup (rust-firecracker org) | https://crates.io/crates/fcnet |
| `hyper-client-sockets` | 0.6.0 (2025-01-20) | hyper over Unix sockets, vsock and Firecracker vsock | https://docs.rs/hyper-client-sockets/0.6.0 |
| `firepilot` / `firec` / `firecracker-rs-sdk` | 1.2.0 (2023) / 0.2.0 (2022) / 0.1.0 (2025) | Older or small Firecracker wrappers; little activity | crates.io |
| `cloud-hypervisor-client` | 0.6.0+api-spec-0.3.0 (2026-05-19) | Client generated from the Cloud Hypervisor REST spec (community, not upstream) | https://crates.io/crates/cloud-hypervisor-client |
| `libkrun` / `krun-sys` | 1.19.7 (2026-10-22) / 1.10.1 (2025-02) | Rust API and FFI to libkrun (containers/libkrun). Use the `stable-*` branch; `main` is the incompatible 2.0 [L1] | https://crates.io/crates/libkrun |
| `msb_krun` | 0.1.42 (2026-10-23) | Native Rust libkrun fork used by microsandbox | https://crates.io/crates/msb_krun |
| `microsandbox` | 0.7.11 (2026-10-24) | Builder-style microVM sandboxes from OCI images on macOS/Linux/Windows; **beta** [L4] | https://crates.io/crates/microsandbox |
| `objc2-virtualization` | 0.3.2 (2025-10-04) | Generated bindings to Apple Virtualization.framework | https://crates.io/crates/objc2-virtualization |
| `tokio-vsock` / `vsock` | 0.7.2 / 0.5.4 | AF_VSOCK sockets for the guest agent and Linux hosts | https://crates.io/crates/tokio-vsock |
| `oci-client` / `oci-spec` | 0.18.0 / 0.10.0 | Pull OCI images and parse image/runtime specs (to build rootfs images for Firecracker) | https://crates.io/crates/oci-client |
| `libcontainer` (youki) | 0.7.0 (2026-07-25) | OCI runtime library in Rust (runs containers *inside* a VM) | https://crates.io/crates/libcontainer |

No official Firecracker or gVisor Rust SDK exists. `runsc` is driven as an OCI runtime through Docker or containerd [G6].

## Guest agent pattern

A VM has no `docker exec`. Something inside the guest must run commands, stream their output and touch files on the host's behalf. Every project below ships one long-lived agent per VM and talks to it over vsock or the VM's network:

| Project | Agent | Transport | Surface |
|---|---|---|---|
| Kata Containers | `kata-agent` (Rust), one per VM [K1] | ttrpc over vsock; also carries stdio [K1] | 42 RPCs, including `CreateContainer`, `ExecProcess`, `SignalProcess`, `WaitProcess`, `WriteStdin`, `ReadStdout`, `CopyFile` [K4] |
| firecracker-containerd | in-VM agent that drives runc inside the VM [F8] | ttrpc through a runtime shim [F8] | Container lifecycle; multi-tenant use "at your own risk" [F8] |
| E2B | `envd` (Go) [E1][E2] | Connect RPC over HTTP, default port 49983, config through Firecracker MMDS [E3] | `Process` (`Start`/`Connect` server-streaming, `StreamInput`, `SendSignal`, `CloseStdin`) and `Filesystem` (`Stat`, `ListDir`, `Move`, `Remove`, `WatchDir`, …) [E4][E5] |
| Apple containerization | `vminitd` as PID 1 [A5] | gRPC over vsock [A5] | Environment setup, process launch, I/O, signals [A5] |

The Firecracker side of the transport: the vsock device is a Unix socket on the host (`uds_path`). For host-to-guest traffic, the host connects to that socket and writes `CONNECT <port>\n`, then gets an ack. For guest-to-host traffic, the guest's connection lands on `<uds_path>_<port>` [F4]. `fctools`' `http_vsock` and `grpc_vsock` extensions and `hyper-client-sockets` implement exactly this [R5].

**How this maps to keelrs.** A keelrs guest agent is a small static Rust binary (tokio + `tokio-vsock` + gRPC or HTTP/2) that implements `SandboxBackend`'s operations in the guest:

- `stat`, `read`, `write`, `write_if`, `remove`, `rename`, `list`, `canonical`, `canonical_all` and `read_files` are each one RPC, with the batched ones staying one round trip.
- `exec` is server-streamed output plus a kill or signal RPC.
- `close` tears the VM down from the host.

The host-side backend becomes a thin client. The same agent can run inside Docker (over TCP or a Unix socket) to test the protocol before any VM exists. That puts the agent seam at the `SandboxBackend` boundary, the same place `docker()` and `remote` already sit in `docs/sandbox.md`.

## macOS dev story

| Option | Runs natively on Apple silicon? | Notes |
|---|---|---|
| Docker Desktop + bollard | Yes, inside a Linux VM ("Docker VMM") [D5] | Zero friction; the Linux kernel is the VM's, not macOS's |
| gVisor | No (Linux only [G5]) | Usable only inside a Linux VM |
| Firecracker / Cloud Hypervisor / Kata | No (need Linux + KVM [F2][C1]) | Run inside a Lima VM with `nestedVirtualization: true`, supported with `vmType: vz` or `krunkit` on **Apple M3 or later, macOS 15 or later** [M1]; Virtualization.framework exposes this from macOS 15 (`isNestedVirtualizationSupported`) [A4]. On M1/M2 use a remote Linux box |
| libkrun / krunvm / microsandbox | **Yes** (HVF on ARM64) [L1][L3][L4] | One code path on Mac and Linux, but a different VMM from Firecracker |
| Apple Virtualization / `containerization` | **Yes** | vsock (macOS 11+), virtio-fs (macOS 12+), save/restore (macOS 14+) [A1–A3]; `containerization` needs macOS 26 [A5]; Swift-first, Rust only through `objc2-virtualization` |

## Recommendation

**Stage 1: `docker()` backend through bollard, now.**

- Implement `SandboxBackend` against Docker's API:
  - `exec` uses `create_exec`/`start_exec` with streamed attach.
  - The file operations use exec'd helpers, or `upload_to_container`/`download_from_container` tar streams.
  - `close` removes the container.
- Harden the defaults: `--network none` unless asked, dropped capabilities, the default seccomp profile, user namespaces or rootless, cgroup limits, a read-only rootfs plus a writable work dir [D1][D2][D4].
- On Linux servers, set the runtime to `runsc`. That is the same bollard code with a gVisor boundary and no KVM needed [G1][G6].
- Why: it works the same on the Mac and on any Linux host, `bollard` is the most mature crate here, and it proves the backend seam.

**Stage 2: a guest-agent protocol, still on Docker.**

- Write the in-guest Rust agent (see [Guest agent pattern](#guest-agent-pattern)) and run it inside the Docker container over TCP or a Unix socket.
- This moves the batched operations (`canonical_all`, `read_files`) and exec streaming onto your own protocol. It is the protocol a VM needs, tested without a VM.

**Stage 3: a `firecracker()` backend on Linux/KVM.**

- Drive the VMM with `fctools`, or a thin client of your own over the Firecracker API socket.
- Start it through the `jailer` [F6], network it with `fcnet` or TAP plus nftables [F5], and reach the agent with `CONNECT <port>` over the vsock UDS [F4].
- Build the rootfs ext4 images from OCI images (`oci-client` → unpack → `mkfs.ext4`) [F7].
- Later, add pre-booted snapshots for fast starts [F3]. This is the path E2B's open-source runtime takes [E1].
- Host on bare metal, or on nested-virt-capable cloud VMs (GCE; EC2 *7i/*8i families) [N1][N2].

**Optional:** a libkrun or microsandbox backend for native macOS VM isolation, if the Mac must run the stronger boundary locally [L1][L4].

**Risks:**

- **Docker is not a hostile-code boundary.** It shares the kernel, and access to the Docker daemon is effectively root on the host [D1]. Never mount the socket into a sandbox. Use gVisor before running untrusted tenants on shared hosts.
- **Firecracker has no virtio-fs**, so files move only through the agent or block images [F6]. The work-dir model must be agent-mediated, which suits keelrs, since `Sandbox::place` already confines paths on the host.
- **Snapshot hazards:** a snapshot restored more than once shares IDs and randomness [F3], and restore needs the same CPU and host [F3]. Snapshot files are trusted input, so keep them out of tenants' reach.
- **Operational load:** TAP and NAT need root [F5], and there is a jailer, kernel and rootfs pipeline to maintain. Kata and firecracker-containerd hide some of this but add containerd plugins and shims [F8][K3].
- **Crate maturity:** `fctools` calls itself "semi-stable" and pins kernel ranges [R4]. `cloud-hypervisor-client` is community-generated. libkrun `main` is an incompatible 2.0 [L1], and microsandbox is beta [L4]. Budget for owning a thin client.
- **The Mac cannot run KVM backends without M3+ and macOS 15+** nested virtualization [M1]. Otherwise CI and a Linux dev box carry Stage 3 testing.

## Sources

- [D1] Docker Engine security: https://docs.docker.com/engine/security/
- [D2] Docker seccomp profiles: https://docs.docker.com/engine/security/seccomp/
- [D3] `docker checkpoint` (experimental, CRIU): https://docs.docker.com/reference/cli/docker/checkpoint/
- [D4] Docker network drivers: https://docs.docker.com/engine/network/drivers/none/ , https://docs.docker.com/engine/network/drivers/bridge/
- [D5] Docker Desktop for Mac install (Docker VMM): https://docs.docker.com/desktop/setup/install/mac-install/
- [G1] gVisor platforms: https://gvisor.dev/docs/architecture_guide/platforms/
- [G2] gVisor security model: https://gvisor.dev/docs/architecture_guide/security/
- [G3] gVisor checkpoint/restore: https://gvisor.dev/docs/user_guide/checkpoint_restore/
- [G4] gVisor performance guide: https://gvisor.dev/docs/architecture_guide/performance/
- [G5] gVisor home ("runs anywhere Linux does"): https://gvisor.dev/
- [G6] gVisor install (runsc as Docker/containerd runtime): https://gvisor.dev/docs/user_guide/install/
- [G7] gVisor networking: https://gvisor.dev/docs/user_guide/networking/
- [F1] Firecracker SPECIFICATION: https://github.com/firecracker-microvm/firecracker/blob/main/SPECIFICATION.md
- [F2] Firecracker README: https://github.com/firecracker-microvm/firecracker/blob/main/README.md
- [F3] Firecracker snapshot support: https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md
- [F4] Firecracker vsock: https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md
- [F5] Firecracker network setup: https://github.com/firecracker-microvm/firecracker/blob/main/docs/network-setup.md
- [F6] Firecracker design (devices, jailer, seccomp): https://github.com/firecracker-microvm/firecracker/blob/main/docs/design.md
- [F7] Firecracker rootfs and kernel setup: https://github.com/firecracker-microvm/firecracker/blob/main/docs/rootfs-and-kernel-setup.md
- [F8] firecracker-containerd: https://github.com/firecracker-microvm/firecracker-containerd
- [C1] Cloud Hypervisor README: https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/README.md
- [C2] Cloud Hypervisor API: https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/api.md
- [C3] Cloud Hypervisor virtio-fs / vsock / snapshot docs: https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/fs.md , https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/vsock.md , https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/docs/snapshot_restore.md
- [K1] Kata architecture: https://github.com/kata-containers/kata-containers/blob/main/docs/design/architecture/README.md
- [K2] Kata hypervisors: https://github.com/kata-containers/kata-containers/blob/main/docs/hypervisors.md
- [K3] Kata with Firecracker (block device/devmapper): https://github.com/kata-containers/kata-containers/blob/main/docs/how-to/how-to-use-kata-containers-with-firecracker.md
- [K4] kata-agent protocol: https://github.com/kata-containers/kata-containers/blob/main/src/libs/protocols/protos/agent.proto
- [L1] libkrun README: https://github.com/containers/libkrun/blob/main/README.md
- [L2] libkrun security model: https://github.com/containers/libkrun/blob/main/docs/security-model.md
- [L3] krunvm: https://github.com/containers/krunvm
- [L4] microsandbox: https://github.com/superradcompany/microsandbox
- [A1] VZVirtioSocketDevice (macOS 11): https://developer.apple.com/documentation/virtualization/vzvirtiosocketdevice
- [A2] VZVirtioFileSystemDevice (macOS 12): https://developer.apple.com/documentation/virtualization/vzvirtiofilesystemdevice
- [A3] VZVirtualMachine save/restore (macOS 14): https://developer.apple.com/documentation/virtualization/vzvirtualmachine/savemachinestateto(url:completionhandler:)
- [A4] VZGenericPlatformConfiguration nested virtualization (macOS 15): https://developer.apple.com/documentation/virtualization/vzgenericplatformconfiguration/isnestedvirtualizationsupported
- [A5] apple/containerization: https://github.com/apple/containerization
- [A6] apple/container: https://github.com/apple/container
- [M1] Lima default template, `nestedVirtualization`: https://github.com/lima-vm/lima/blob/master/templates/default.yaml
- [E1] E2B infra: https://github.com/e2b-dev/infra
- [E2] envd: https://github.com/e2b-dev/infra/tree/main/packages/envd
- [E3] envd main.go (port 49983, Connect RPC, MMDS): https://github.com/e2b-dev/infra/blob/main/packages/envd/main.go
- [E4] envd process.proto: https://github.com/e2b-dev/infra/blob/main/packages/envd/spec/process/process.proto
- [E5] envd filesystem.proto: https://github.com/e2b-dev/infra/blob/main/packages/envd/spec/filesystem/filesystem.proto
- [N1] GCE nested virtualization: https://docs.cloud.google.com/compute/docs/instances/nested-virtualization/overview
- [N2] EC2 nested virtualization: https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/amazon-ec2-nested-virtualization.html
- [P1] Fly.io architecture ("Application code runs in Firecracker microVMs"): https://docs.fly.io/reference/architecture
- [P2] Modal Sandboxes (gVisor): https://modal.com/docs/guide/sandboxes
- [R1] bollard docs: https://docs.rs/bollard/0.21.1/bollard/
- [R2] bollard `Docker` methods: https://docs.rs/bollard/0.21.1/bollard/struct.Docker.html
- [R3] containerd rust-extensions: https://github.com/containerd/rust-extensions
- [R4] fctools README: https://github.com/kanpov/fctools
- [R5] fctools docs: https://docs.rs/fctools/0.6.0/fctools/
- Crate versions: crates.io API, `https://crates.io/api/v1/crates/<name>`, queried 2026-10-29.
