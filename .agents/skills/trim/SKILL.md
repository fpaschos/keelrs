---
name: trim
description: "Cut code and comments in the current diff while keeping the ticket's and existing behaviour."
disable-model-invocation: true
---

Less code, less liability: every line is something to read, test and maintain. Cut the code this branch adds or changes (`git diff main...HEAD`) without changing behaviour; when a cut is a close call, delete.

First find the ticket the branch implements, taking the first that turns one up: the user's argument, the conversation so far, issue references in the branch's commit messages (`Refs #6`, `Closes #6`), the branch name, the open PR's description. Fetch it and list its acceptance criteria; those and all existing tests must still hold. If no ticket turns up, ask; with none, the tests are the only bar.

Read each changed file whole for context, but cut only lines in the diff. Hunt these smells (Fowler's names, each a judgement call; the repo's documented code style overrides any line saving):
- **Speculative Generality** (YAGNI): code the ticket does not require, such as checks, heuristics, parameters, or guards against your own repo's scripts.
- **Dead Code**: unused imports, exports, branches and parameters.
- **Duplicated Code**: two fields, helpers or branches that do the same thing; merge them when the merge is shorter.
- **Reuse**: search the codebase for an existing helper that does what new code does; call it instead.
- **Lazy Element** and **Middle Man**: a helper used once, or one that only delegates. Apply the deletion test: inline it when deleting it just moves its lines into one caller.
- **Feature Envy**: logic at the wrong altitude; move it into the helper or module whose data it works on.
- **Efficiency**: work repeated per call or per loop that can be done once (module-level values, compiled regexes, cached reads).
- **Comments**: ones that restate the code, or deodorise unclear code that a better name would fix; shorten the rest to one line.

A cut that removes an assertion needs proof the rest still guard its behaviour: break that behaviour by hand once, run the test file, see it go red, revert.

List each cut with its line saving and what behaviour, if any, it drops. Apply the cuts that drop no behaviour at all. Ask about the rest: any cut that drops behaviour, changes something another contract depends on (wire formats, stdin/stdout shapes, public exports), or reverses a change made for review.

Run `cargo clippy --all-targets` and `cargo test` after the cuts, in the root and with `--manifest-path examples/Cargo.toml`. Leave the cuts uncommitted for the user to review; note any PR description a cut makes stale.
