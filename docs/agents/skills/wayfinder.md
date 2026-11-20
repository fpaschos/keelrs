# Wayfinder in keelrs

Repo rules for `/wayfinder`. They add to the upstream skill; where the two disagree, this file wins. Grilling rules are in `grilling.md`, implementation rules in `implement.md` and `AGENTS.md` ("Code"), review rules in `CODING_STANDARDS.md`.

## The loop

Grill → task → implement → review, then the next grilling. A grilling's task is worked before the next grilling starts; the open grillings wait on it. Work a `wayfinder:task` with `/implement`, so `/code-review` runs on it.

## The task ticket

The task body is the spec: standalone and the single source. The grilling's resolution comment links to it and holds no second copy. The body carries:

- the agreed code sketches;
- third-party signatures, from the pinned version's source;
- the examples before and after, and the call sites to migrate;
- the agreed test seams;
- the "No `X`" decisions, verbatim;
- the acceptance, with targets for the `AGENTS.md` success metrics (code lines, `pub` items, tests);
- a last line `Open: none`. A task with an open question goes back to its grilling.

A feature's last ticket is a task on the real codebase. A prototype comes first only for a new API whose Rust ergonomics are untested.

## Review and close

- A review that changes a decision rewrites the grilling resolution and the task body in place, whole, to match the code, before the merge.
- A PR closes the task it builds with `Closes #<n>`.
- The PR reports the metrics before and after against the ticket's targets. A miss in the wrong direction is a question to the user before the merge.

## Tracker writes

Show a numbered plan and wait for a go before you create, close or edit a ticket or the map. Every new ticket names the Destination step it serves; a ticket that serves none goes to Out of scope.
