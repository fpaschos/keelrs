# Two workspaces, one dependency table each

The repo has two Cargo workspaces, as axum does: the root (the `keelrs` crate) and `examples/` (its own `Cargo.lock`). Examples can pull in heavy runtime crates (`tokio` `full`, `anyhow`) without them ever entering the library's lock, its `cargo deny` graph, or its build. Each workspace keeps every version in a `[workspace.dependencies]` table and its crates use `workspace = true`, so a pin has one home per workspace; `cargo deny`'s `[bans.workspace-dependencies]` fails on a duplicate or unused entry.

## Consequences

- `cargo add` can't write to `[workspace.dependencies]`: a new pin is seeded by hand, then `cargo add -p <crate> <name>` (no version) inherits it. `cargo remove` deletes every unused entry, so seed and add together.
- `cargo add` can't add the `keelrs` path dependency to an example (it resolves `keelrs`'s `*.workspace` keys against the examples workspace); `keelrs.workspace = true` is written by hand.
- Every command runs twice, the second time with `--manifest-path examples/Cargo.toml`.
