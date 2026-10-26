# Exact `=` version pins

Every dependency in both workspaces is pinned with an exact `=` requirement, including in the `keelrs` library, where caret ranges are the norm. The pin is a reviewed choice: a version changes only when someone runs `cargo upgrade --pinned` and reads the result, never as a side effect of `cargo update` or a fresh lock. The cost falls on downstream users, who can't unify a pinned crate with a different version of it, so the library's pins are revisited before keelrs is first published.

## Consequences

- `cargo update` leaves pinned crates alone but still moves their transitive dependencies; `Cargo.lock` stays the real lock and is committed.
- `cargo upgrade` skips pins unless given `--pinned`.
