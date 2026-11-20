# Wayfinder in keelrs

Rules for every `/wayfinder` step on a keelrs map, and for the grilling, task and review it runs. They add to the upstream skills; where the two disagree, this file wins. Implementation rules live in `AGENTS.md` ("Code"); review rules in `CODING_STANDARDS.md`.

## The loop

Grill → task → implement → review, then the next grilling. A grilling's task is worked before the next grilling starts; the open grillings wait on it.

## Grilling

- Ask one question per turn: ❓ the question, the options, ➡️ the recommendation and its source (a ticket, an ADR, research, or "my reading of `<file>`").
- Open each question with what the ticket and earlier resolutions already say, and the current code it touches.
- Recommend the least general option that serves the ticket now.
- Answer a "why" or "how" question with an explanation, not options.
- Turn a recommendation with no source into a research ticket before it is decided.
- The grilling is done when:
  - the user has approved the code that uses the design: the caller's loop, not only the library API;
  - every signature in the sketch is checked against `main` and the pinned crates' source;
  - every question asked has an answer.

  A letter answer settles a name. State, concurrency, types and contracts (see `AGENTS.md` "Code") need approved code.

## The task ticket

The task body is the spec: standalone and the single source. The grilling's resolution comment links to it and holds no second copy. The body carries:

- the agreed code sketches;
- third-party signatures, from the pinned version's source;
- the examples before and after, and the call sites to migrate;
- the tests and their seams;
- the "No `X`" decisions, verbatim;
- the acceptance, with targets for the `AGENTS.md` success metrics (code lines, `pub` items, tests).

A feature's last ticket is a task on the real codebase. A prototype comes first only for a new API whose Rust ergonomics are untested.

## Review and close

- A review that changes a decision rewrites the grilling resolution and the task body in place, whole, to match the code, before the merge.
- PRs say `Refs #<n>`; the user closes tickets (see `issue-tracker.md`).
- The PR reports the metrics before and after against the ticket's targets. A miss in the wrong direction is a question to the user before the merge.

## Tracker writes

Show a numbered plan and wait for a go before you create, close or edit a ticket or the map. Every new ticket names the Destination step it serves; a ticket that serves none goes to Out of scope.
