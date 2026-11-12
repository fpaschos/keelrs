# The hunt

The user spotted one instance of a smell. The **hunt** finds its **siblings**: every other place in the change with the same smell, so one decision fixes them all.

## Smells

The baseline is the smell list in [`../../trim/SKILL.md`](../../trim/SKILL.md). A walkthrough meets these most often:

- **Duplicated work across layers**: two layers build or hold the same data (a caller stamps a record, the callee checks the stamping; the same struct built in code and in tests). Fix: one layer owns it, so the check has nothing to catch.
- **Over-protection**: a guard for a state no path reaches (an early return for a case the caller already ruled out, a branch only another guard keeps alive, an error invented for what would be a bug). Fix: let the types carry it, or state the invariant once.
- **Round trips**: a value converted and converted back (an error wrapped in a wider type, then unwrapped by its only reader). Fix: return the narrow type.
- **Out of line**: a file, name or layout that differs from its siblings without reason (one module laid out unlike the rest, one file holding several concerns). Fix: follow the siblings; split by what the code holds.
- **Copied logic in tests**: a test helper that reimplements production code, so the test checks the copy. Fix: call the production code; share one helper between test modules.
- **Restating tests**: two tests that cover one behaviour, or a test of a setter or a derive. Fix: keep the one at the seam that reaches the behaviour.

## Procedure

1. **Name** the smell in one sentence, with the user's instance as the example.
2. **Dispatch** one subagent per smell, in parallel when there are several. Brief: the smell sentence, the example, the list of files in the change, and "return every place with this smell as `file:line`, what it costs, and whether removing it would change behaviour; read each file whole".
3. **Verify** every hit by reading it yourself; drop the ones that are not the smell.
4. **Sort** the hits: worth changing, worth keeping (with the reason: a real invariant, atomicity, a contract), and low value.
5. **Report**, then propose: the options, what each saves in the `AGENTS.md` metrics, what it changes in a contract (public API, stored data), and your recommendation.

Done when the smell has been checked against every file in the change and every hit is sorted.
