# Grilling in keelrs

Repo rules for `/grilling`, and for `/grill-with-docs` and `/wayfinder`, which run it. They add to the upstream skill; where the two disagree, this file wins.

- Ask one question per turn: ❓ the question, the options, ➡️ the recommendation and its source (a ticket, an ADR, research, or "my reading of `<file>`").
- Open each question with what the ticket and earlier resolutions already say, and the current code it touches.
- Recommend the least general option that serves the ticket now.
- Answer a "why" or "how" question with an explanation, not options.
- Turn a recommendation with no source into a research ticket before it is decided.
- Record a hard "No `X`" decision (hard to reverse, a real trade-off) as an ADR, so `/tdd` reads it while it builds.
- The grilling is done when:
  - the user has approved the code that uses the design: the caller's loop, not only the library API;
  - every signature in the sketch is checked against `main` and the pinned crates' source;
  - every question asked has an answer.

  A letter answer settles a name. State, concurrency, types and contracts (see `AGENTS.md` "Code") need approved code.
