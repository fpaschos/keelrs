# The hunt

The user spotted one instance of a smell. The **hunt** finds its **siblings**: every other place in the change with the same smell, so one decision fixes them all.

## Smells

The baseline is the smell list in [`../../trim/SKILL.md`](../../trim/SKILL.md) plus the repo's `CODING_STANDARDS.md`; where they disagree, the repo file wins. Name each hit by its smell or by its standard's heading.

## Procedure

1. **Name** the smell in one sentence, with the user's instance as the example.
2. **Dispatch** one subagent per smell, in parallel when there are several. Brief: the smell sentence, the example, the list of files in the change, and "return every place with this smell as `file:line`, what it costs, and whether removing it would change behaviour; read each file whole".
3. **Verify** every hit by reading it yourself; drop the ones that are not the smell.
4. **Sort** the hits: worth changing, worth keeping (with the reason: a real invariant, atomicity, a contract), and low value.
5. **Report**, then propose: the smell or standard each hit breaks, the options, what each saves in the `AGENTS.md` metrics, what it changes in a contract (public API, stored data), and your recommendation.

Done when the smell has been checked against every file in the change and every hit is sorted.
