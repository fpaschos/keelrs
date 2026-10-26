# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's issue tracker.

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `agent-ready`        | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `human-ready`        | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |
| _(keelrs-specific)_         | `agent-complete`     | Agent finished its step; open, waiting on the user |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

Edit the "Label in our tracker" column to match whatever vocabulary you actually use.

## `agent-complete`

Not a skill role. Set when an agent has finished its part of the workflow on a ticket (implementation, review, a
merge, or any other operation) and the next step needs the user: review, coordination, or a decision. The issue
stays **open**. The user decides what happens next: close it, re-queue it (`agent-ready`), or hand it over
(`human-ready`), removing `agent-complete`.

Lifecycle: `agent-ready` → `agent-complete` (open) → user decides. Human work: `human-ready` → closed.
