---
name: commit
description: Create an atomic commit for current changes. Use when the user wants to commit pending work with a conventional-commit message.
model: sonnet
---

# Commit Changes

## Process

### 1. Review Changes

```bash
git status
git diff HEAD
git diff --stat HEAD
git ls-files --others --exclude-standard
```

Calibrate on the repo's voice before drafting:

```bash
git log -10 --format='%h %s%n%n%b' | head -80
```

### 2. Stage Files

Stage only files relevant to the current logical change. One commit = one logical change.

**Do NOT stage:**

- `.env` or credential files
- Large binary files
- Files unrelated to the current task

If the diff spans multiple unrelated concerns, stop and split into separate commits.

### 3. Write the Message

**Format:**

```
tag(scope): concise description

<Context paragraph>
Lead with the problem, prior behavior, or motivation. 1–3 sentences,
wrapped at 72 cols. This is the WHY.

<Change paragraph or bullet list>
What changed, in concrete terms. Use a bullet list when enumerating
multiple files, symbols, or steps; use a paragraph for a single
cohesive change.

- path/to/file.rs: what changed and why this entry exists
- symbol or area: one-line note
- ...

<Optional closing line>
Trailing rationale, follow-up note, or invariant preserved
(e.g. "Pure code motion — no assertions changed").
```

Skip the body entirely if the subject says it all (releases, trivial
renames, one-line fixes). Otherwise the body should have at least the
context paragraph.

**Tags:** `feat` `fix` `refactor` `docs` `test` `chore` `perf`

**Scopes in this repo:** crate scopes (`keelrs`, `examples`) or doc scopes (`docs`, `agents`). Omit scope for repo-wide changes (e.g. `docs:`, `chore:`).

**Rules:**

- Subject ≤72 chars, imperative (`add`, `drop`, `rename` — not `added`/`adds`). Match the case used in recent commits.
- Body wraps at 72 cols.
- **Always open with a context paragraph** stating the problem, prior behavior, or motivation — unless the subject is fully self-evident.
- **Use a bullet list** when the change touches multiple files, symbols, or steps. Prefix each bullet with the path or area (`keelrs/src/...:`, `docs/...:`, `§3.2 lifecycle:`).
- Cite concrete symbols, file paths, and counts (function names, file paths, `42 pass / 1 skip`).
- Don't restate the diff in prose — context + bullets is denser.
- No marketing words (`comprehensive`, `robust`, `enhanced`, `improved`). No `this commit`. No trailing summary that just recaps the subject.
- Never add `Co-Authored-By` or any AI attribution.

### 4. AI Context Files

If files under `.claude/` / `.agents/` or `CLAUDE.md` / `AGENTS.md` are part of the change, mention them naturally in the body alongside everything else — no special `Context:` section.
