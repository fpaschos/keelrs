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

- **Example readability**: `examples/` is what an end user reads first; each example stays short and plain, with the API doing the work.
- **Lean code**: fewer lines and fewer concepts in `keelrs` and `examples/`; count the lines.

## Cargo

Change manifests and dependencies only through `cargo`, never by hand-editing `Cargo.toml` or `Cargo.lock`.

- Dependencies: `cargo add` / `cargo remove` (with `-p <crate>` or `--manifest-path`, `--features`, `--dev`, `--build`).
- Pins live only in each workspace's `[workspace.dependencies]`, as exact `=` versions (ADR 0001, 0002). A new crate: seed its pin there by hand (ask first, as below), then `cargo add -p <crate> <name>` with **no version**, so it writes `workspace = true`. Seed and add together: `cargo remove` deletes every unused workspace entry.
- `keelrs` in an example: hand-write `keelrs.workspace = true`; `cargo add` fails on it.
- New crates or examples: `cargo new` / `cargo init` (e.g. `cargo new --vcs none examples/<name>`).
- Lock file: `cargo update` / `cargo generate-lockfile`; never edit `Cargo.lock` directly.
- Upgrades: `cargo outdated -R` to see what's behind; `cargo upgrade --pinned` (from `cargo-edit`, installed) to raise the pins, `--dry-run` first, `--incompatible` for breaking bumps; `cargo set-version` for crate versions.
- Two workspaces: the root and `examples/` (own `Cargo.lock`). Run each command for both, the second with `--manifest-path examples/Cargo.toml`.
- If a change has no `cargo` command (e.g. workspace lints or `[workspace.package]`), ask the user before editing the manifest by hand.

## Checks

Run before you call a change done, once per workspace (the second time add `--manifest-path examples/Cargo.toml`); all must pass:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo deny check   # examples: cargo deny --manifest-path examples/Cargo.toml check
```

Lints are `warn` in the manifests; `-D warnings` here is the gate. A change to `keelrs` can stale `examples/Cargo.lock`: refresh it with `cargo update --manifest-path examples/Cargo.toml -p keelrs` before the locked examples checks.
