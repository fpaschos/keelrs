---
name: trim
description: "Cut code and comments in the current diff, or in a named path, while keeping the ticket's and existing behaviour."
disable-model-invocation: true
---

Less code, less liability: every line is something to read, test and maintain. Cut without changing behaviour; when a cut is a close call, delete. The target is the code this branch adds or changes, committed or not (`git diff $(git merge-base main HEAD)`); when the argument names a path, module or item, the target is that code, whole, whether or not the branch touched it.

For a diff target, first find the ticket the branch implements, taking the first that turns one up: the user's argument, the conversation so far, issue references in the branch's commit messages (`Refs #6`, `Closes #6`), the branch name, the open PR's description. Fetch it and list its acceptance criteria; those and all existing tests must still hold. If no ticket turns up, ask; with none, and for a named target, the tests are the only bar.

Read each target file whole for context, but cut only lines in the target. Hunt these smells (Fowler's names, each a judgement call; the repo's documented code style overrides any line saving):
- **Speculative Generality** (YAGNI): code the ticket does not require, such as checks, heuristics, parameters, or guards against your own repo's scripts.
- **Dead Code**: unused imports, exports, branches and parameters.
- **Duplicated Code**: two fields, helpers or branches that do the same thing; merge them when the merge is shorter.
- **Reuse**: hand-rolled code that something already in reach does: a helper in this codebase, the language's standard library, or a dependency already in the tree. Read the replacement's source at the version the repo pins and call it instead; every way its semantics differ (ordering, ties, error cases) is dropped behaviour.
- **Lazy Element** and **Middle Man**: a helper used once, or one that only delegates. Apply the deletion test: inline it when deleting it just moves its lines into one caller.
- **Feature Envy**: logic at the wrong altitude; move it into the helper or module whose data it works on.
- **Efficiency**: work repeated per call or per loop that can be done once (module-level values, compiled regexes, cached reads).
- **Comments**: ones that restate the code, or deodorise unclear code that a better name would fix; shorten the rest to one line.

The hunt is done when every smell has been checked against every target file. A cut that removes an assertion needs proof the rest still guard its behaviour: break that behaviour by hand once, run the test file, see it go red, revert. For a named target, each cut names the test that covers its behaviour; a cut with none is asked about.

List each cut and what behaviour, if any, it drops, then the smells that found nothing. Report the AGENTS.md lean-code counts before and after. Apply the cuts that drop no behaviour at all. Ask about the rest: any cut that drops behaviour, adds a dependency, changes something another contract depends on (wire formats, stdin/stdout shapes, public exports), or reverses a change made for review.

Run the AGENTS.md checks in both workspaces after the cuts. Leave the cuts uncommitted for the user to review; note any PR description a cut makes stale.
