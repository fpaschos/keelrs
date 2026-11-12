# The tour

One page the user keeps open beside the code: a published page when the host offers one (an artifact, a doc), else a single HTML file. It is **living**: it always describes the head.

## Find

The PR description holds the tour's link on a line of its own: `Tour: <link>`. Take the link from the argument, else from that line. If there is none, or the page is gone or unreadable, **Build** a new one; otherwise **Sync** it. After building, write the `Tour:` line into the PR description, replacing any old one, so the next walkthrough finds it.

## Build

1. **Map** the change with a subagent. Brief: the diff command, the ticket, and "return every changed file with what it holds, the symbols a reader must meet with their `file:line` at the head, and which files change together". Verify a sample of the lines it returns.
2. **Order** the stops outside-in: from where a user of the code meets the change (its entry point, its public API) to its internals, then the tests last. One stop per concern, not per file; a file may appear in two stops.
3. **Write** each stop:
   - **Links**: every symbol the stop discusses, as a link to the code at the head SHA (`…/blob/<sha>/<path>#L<n>`), labelled `path:line name`, opening in a new tab.
   - **Body**: what the code does and why, naming the decision (ticket, ADR, glossary term) behind it. Show the few lines that carry the idea, not the file.
   - **Why it looks like this**: the design reason, when the code alone does not say it.
   - **Deviation**: where the code departs from the ticket, and why.
   - **Check yourself**: one question that needs the stop to answer, with the answer hidden until clicked.
4. **Frame** the stops with: a header (PR, head SHA, ticket, size), the terms a reader needs, a map of where the code lives (file → what it holds → stops), the commit list, and the metrics `AGENTS.md` defines, before and after.

Done when every changed file belongs to a stop.

## Sync

After commits move the head:

1. **Re-map** with a subagent. Brief: the old and new SHA, the tour's list of `path:line name` links, and "return each link's path and line at the new head, by finding the same symbol; mark the ones whose symbol is gone".
2. Rewrite every link to the new SHA with its new path and line; rewrite or drop the ones whose symbol is gone.
3. Rewrite the text of every stop a commit touched, so it describes the code as it now is; add a stop for a new concern.
4. Update the header, the map, the commit list and the metrics.
5. **Check** the links with a subagent. Brief: the link list, and "fetch each at the head and return the ones that do not resolve". Fix those.

Done when every link resolves at the head and no stop describes code that has changed.
