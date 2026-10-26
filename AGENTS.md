# AGENTS.md

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues on `fpaschos/keelrs`, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default role names, except `ready-for-agent` → `agent-ready` and `ready-for-human` → `human-ready`; plus `agent-complete` (agent finished its step, issue open, waiting on the user). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root, shared by all crates. See `docs/agents/domain.md`.

## Cargo

Change manifests and dependencies only through `cargo`, never by hand-editing `Cargo.toml` or `Cargo.lock`.

- Dependencies: `cargo add` / `cargo remove` (with `-p <crate>` or `--manifest-path`, `--features`, `--dev`, `--build`).
- New crates or examples: `cargo new` / `cargo init` (e.g. `cargo new --vcs none examples/<name>`).
- Lock file: `cargo update` / `cargo generate-lockfile`; never edit `Cargo.lock` directly.
- Upgrades: `cargo outdated -R` to see what's behind; `cargo upgrade` (from `cargo-edit`, installed) to raise versions in `Cargo.toml`, `--incompatible` for breaking bumps; `cargo set-version` for crate versions.
- Two workspaces: the root and `examples/` (own `Cargo.lock`). Run each command for both, the second with `--manifest-path examples/Cargo.toml`.
- If a change has no `cargo` command (e.g. workspace lints or `[workspace.package]`), ask the user before editing the manifest by hand.
