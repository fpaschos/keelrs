# Coding standards

Judgement rules a reviewer applies to a diff. `/code-review` reads this file; so do `/trim` and `/walkthrough`'s hunt, on top of their own smell lists. Mechanical rules live in tooling (`.clippy.toml`, `deny.toml`, the `AGENTS.md` checks); the rules for how to build live in `AGENTS.md`. Cite a rule by its heading.

## Ticket fidelity

The diff builds what the ticket and the decisions it links settle (`AGENTS.md` "Code").

- Flag every new or changed state, concurrency, type, name or contract that the ticket, an ADR, `GLOSSARY.md` or a linked resolution does not settle, unless the PR shows the user's approval.
- Flag every item of the ticket's code sketch that the diff leaves out.
- Every "No `X`" decision in the ticket or its resolution holds: `X` is absent from the diff.
- A changed doc comment on a `pub` item is a contract change.

## Own abstractions

Like things are built alike, on keelrs's own abstractions: a keelrs-owned tool is a plain `Tool` (`Tool::host` or `Tool::sandbox`) living with the other built-in tools, as `activate_skill` and `finish_structured` are. Flag a parallel path: a second way to do what an existing abstraction does.

## Ownership

One layer owns each piece of data.

- **Duplicated work across layers**: two layers build or hold the same data (a caller stamps a record and the callee checks the stamp; the same struct built in code and in tests). Fix: one layer owns it, so the check has nothing to catch.
- **Round trips**: a value converted and converted back (an error wrapped in a wider type, then unwrapped by its only reader). Fix: return the narrow type.

## Guards

**Over-protection**: a guard for a state no path reaches (an early return for a case the caller already ruled out, a branch only another guard keeps alive, an error invented for what would be a bug). Fix: let the types carry it, or state the invariant once.

## Layout

**Out of line**: a file, name or layout that differs from its siblings without reason (one module laid out unlike the rest, one file holding several concerns). Fix: follow the siblings; split by what the code holds.

## Tests

Tests follow the design, never shape it.

- Flag a type, field or seam that exists so a test can reach state; a behaviour no agreed seam reaches waits for one, or goes untested for now.
- **Copied logic in tests**: a test helper that reimplements production code (a loop, a fold), so the test checks the copy. Fix: call the production code; share one helper between test modules.
- **Restating tests**: two tests that cover one behaviour, a test that restates the code, or a test of a setter or a derive. Fix: keep the one at the agreed seam that reaches the behaviour.
- New tests sit where their siblings do: `keelrs/tests/it/` for behaviour through the public API, `#[cfg(test)]` in the module only for a `pub(crate)` seam the user agreed.

## Metrics

The PR reports the `AGENTS.md` success metrics before and after. Flag a result that moves away from the ticket's target, such as more lines where the ticket expects fewer, or more `pub` items than its sketch.

## Words

Names and docs use `GLOSSARY.md` terms as it defines them. Flag a term the glossary lists under _Avoid_ in code, docs or ADRs.
