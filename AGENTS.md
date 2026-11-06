# AGENTS.md

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues on `fpaschos/keelrs`, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default role names, except `ready-for-agent` → `agent-ready` and `ready-for-human` → `human-ready`; plus `agent-complete` (agent finished its step, issue open, waiting on the user). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root, shared by all crates. See `docs/agents/domain.md`.

## Success metrics

Judge every API-shaping change on two metrics, before and after, and report both:

- **Example readability**: `examples/` is what an end user reads first. Count each example's **boilerplate**: lines every user's program would repeat (adapters, wrappers, wiring), which belong in the API.
- **Lean code**: count the code lines (`tokei keelrs/src` and `tokei examples`, the Code column) and the **public surface**: `pub` items in `keelrs` plus the `keelrs` names an example must use.

When they conflict, examples win: move boilerplate into `keelrs` as long as its public surface grows less than the boilerplate shrinks.

## Cargo

Change manifests and dependencies only through `cargo`, never by hand-editing `Cargo.toml` or `Cargo.lock`.

- Dependencies: `cargo add` / `cargo remove` (with `-p <crate>` or `--manifest-path`, `--features`, `--dev`, `--build`).
- Pins live only in each workspace's `[workspace.dependencies]`, as exact `=` versions (ADR 0001, 0002). A new crate: seed its pin there by hand (ask first, as below), then `cargo add -p <crate> <name>` with **no version**, so it writes `workspace = true`. Seed and add together: `cargo remove` deletes every unused workspace entry.
- `keelrs` in an example: hand-write `keelrs.workspace = true`; `cargo add` fails on it.
- New crates or examples: `cargo new` / `cargo init` (e.g. `cargo new --vcs none examples/<name>`).
- Lock file: `cargo update` / `cargo generate-lockfile`.
- Upgrades: `cargo outdated -R` to see what's behind; `cargo upgrade --pinned` (from `cargo-edit`, installed) to raise the pins, `--dry-run` first, `--incompatible` for breaking bumps; `cargo set-version` for crate versions.
- Two workspaces: the root and `examples/` (own `Cargo.lock`). Run each command for both, the second with `--manifest-path examples/Cargo.toml`.
- If a change has no `cargo` command (e.g. workspace lints or `[workspace.package]`), ask the user before editing the manifest by hand.

## Code

Build on keelrs's own abstractions, so like things are built alike: a keelrs-owned tool is a plain `Tool` (`Tool::host` or `Tool::sandbox`) living with the other built-in tools, as `activate_skill` and `finish_structured` are. When one does not fit, generalize it, drop it, or propose a replacement to the user before writing a parallel path.

Build what an approval or a prior decision settles: the ticket, an ADR, `GLOSSARY.md`, an earlier ticket's resolution. Within that, the code is yours: private functions, local refactors, the order of the work. Each of these, unless already decided, is a design decision: propose it with options and build it once the user approves.

- **State**: a new field, or state moved or regrouped between types.
- **Concurrency**: who owns, writes or shares state: a new task, actor, channel, lock or shared value. An actor's state is the common case.
- **Types and names**: a new type, trait or module, or a renamed one.
- **Contracts**: the public API, `Error` variants, and serialized shapes (events, stored data).

Tests follow the design, never shape it. A behaviour no agreed seam reaches waits for one, or goes untested for now; wrapping existing state in a new type so a test can reach it is the antipattern. TDD tests are scaffolding: once the loop is green, review them: unify, move to the agreed public seam, or delete the ones that only restate the code.

## Checks

Run before you call a change done, in both workspaces; all must pass:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo deny check   # examples: cargo deny --manifest-path examples/Cargo.toml check
```

A change to `keelrs` can stale `examples/Cargo.lock`: refresh it with `cargo update --manifest-path examples/Cargo.toml -p keelrs` before the locked examples checks.
