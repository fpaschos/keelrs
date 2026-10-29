# Sandbox backends

A [`Sandbox`](../keelrs/src/sandbox/mod.rs) is a cloneable handle over a `SandboxBackend`: the operations one kind of
sandbox implements (this machine, a container, a microVM, a remote host). `Sandbox` confines paths to the work dir and
checks capabilities before it calls the backend, so a backend only sees absolute, canonical paths inside the work dir.

`local()` implements the backend for this machine (`keelrs/src/sandbox/local.rs`): files on rig-tools, commands on
`tokio::process`. Each command leads a session and process group of its own, with no controlling terminal, and gets
its env directly, never through argv. A shell line runs with `bash -c` (`sh -c` where there is no bash), resolved by
absolute path at `open`. A kill or timeout sends SIGTERM to the group, then SIGKILL 2 s later; once the command exits,
whatever it left in its group has 100 ms to close the output, then the group is killed.

## Operations

| Method | What it does | Why sandboxed tools need it |
|---|---|---|
| `stat` | Says what a path is, links followed: a file (size, read-only, modified), a directory, something else, or missing. A missing path is not an error. | Checking before acting: does it exist, is it a directory, is it too big to read. |
| `read` | Up to `limit` bytes from the start of a regular file, and the file's whole size. Anything but a regular file is refused before it is opened. | Showing the model a file without loading a huge one whole. |
| `write` | Replaces a file in one step (temp file, sync, rename), creating its directory. | Writing files so a crash or a full disk never leaves one half written. |
| `remove` | Removes a file; a directory only when empty, or with `recursive`. | Deleting files and scratch output. |
| `rename` | Moves a file, creating the target's directory and replacing a file there. | Moves and refactors. |
| `list` | The regular files under a root, only those matching a glob when one is given. | Listing files and choosing what to search. |
| `canonical` | A path with its links followed and `..` folded; a missing tail is kept. | Confinement: `Sandbox` canonicalizes every path and refuses one outside the work dir. |
| `exec` | Starts a command (a shell line or argv) with a cwd, env, stdin and timeout; returns its output as it comes, its exit, and a kill. | Running shells, builds, tests, git. |

## Operations with a default

| Method | What it does | Why sandboxed tools need it | Default |
|---|---|---|---|
| `write_if` | Writes only while the file still holds the content hashed as `held`; otherwise writes nothing and returns what it holds now. | Edits made from text the model read earlier must not overwrite newer changes. | Read, compare, write (not atomic). |
| `read_files` | The whole content of each of many files up to a limit; `None` for any other path. | Search reads many files; a remote backend can do it in one round trip. | One `read` per path. |
| `capabilities` | Whether `write` (with `write_if`, `remove`, `rename`) and `exec` are supported. | A read-only or exec-less sandbox refuses those with `Unsupported` before any work. | Everything. |
| `shell` | The shell that runs shell lines, by its path in the sandbox. | The `shell` tool can tell the model which shell it writes for. | `None`: not said. |
| `close` | Removes the sandbox and what runs in it. | A container or remote host has something to tear down; afterwards every clone fails with `Died`. | Nothing. |
