# AGENTS.md

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues on `fpaschos/keelrs`, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default role names, except `ready-for-agent` → `agent-ready` and `ready-for-human` → `human-ready`; plus `agent-complete` (agent finished its step, issue open, waiting on the user). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root, shared by all crates. See `docs/agents/domain.md`.
