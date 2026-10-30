# Built-in tools

The eight sandboxed tools every turn that declares a sandbox gets (`keelrs/src/builtin/`), narrowed with `use_sandbox(sb).only([...])`. They act only through `Sandbox` methods, so they behave alike on every backend ([sandbox.md](sandbox.md)). Their text logic is rig-tools 0.46.0-rc.1's, called where it is public and copied, marked `// Copied from rig-tools`, where it is not.

## The tools and their limits

| Tool | Does | Limits |
|---|---|---|
| `read` | a text file's lines, paged | files over 16 MB refused; 2000 lines or 50 KB a call, then the offset to read on; long lines cut with a note |
| `write` | a file's whole content, creating its folders | — |
| `edit` | exact replacements in one file, in order, all or none | the file as last read, written only if it still holds that text |
| `bash` | a command line in the work dir | last 2000 lines or 50 KB; stopped at 10 MB printed; no timeout unless given |
| `grep` | lines matching a regex, ignore rules applied | 100 matches, 500 characters a line, 100 KB in all; files over 16 MB or not UTF-8 are skipped, and a note names them |
| `glob` | files matching a path glob from the searched directory | 1000 paths or 50 KB |
| `delete` | one file, never a directory | — |
| `move` | one file, creating `to`'s folders | refuses an existing `to` unless `overwrite` |

What a tool cut is dropped, and a note says what was cut and how to see more.

## Exact text, and what `bash` changes

`read`, `grep`, `edit` and `write` see and return a file's text exactly. `bash` changes what a command printed before the model sees it:

- **Secrets redacted**, each line through rig-core's `redact::text` (API keys and tokens by their shape, values of secret-named variables), and a private key block left out through its end line.
- **Terminal escape sequences dropped**: colours, links, titles.

The split is rig-tools' own. A file's text round-trips: `read` then `write`, or `edit`'s `old_text`, would put `[REDACTED]` into the file, or fail to match it, if `read` redacted. A command's output is logs and diagnostics, rarely written back. `Sandbox::exec` stays raw, for code that needs the bytes.

Redaction lowers the odds of a key reaching the model and its provider's logs. It is no boundary: `bash` runs any command, so `base64 .env` prints a key no pattern finds, and `read .env` shows it as it is. Keep secrets out of the sandbox (its environment is an allowlist, [sandbox.md](sandbox.md)); don't rely on redaction to hide them.

## Deliberate differences from rig-tools

- **Confinement is always on**, also for `grep`'s reads, and a dangling link is followed to where a write would create the file before it is checked. rig-tools' confinement is optional.
- **The environment is an allowlist.** `SSH_AUTH_SOCK`, proxy variables, `CARGO_HOME`, `JAVA_HOME` and the like are not passed through; add them with `local().env(..)`.
- **`..` folds before links resolve.** `link/../x` names `x` beside `link`, where the shell names it beside `link`'s target. It never leads out of the work dir either way.
- **A link is acted on as its target.** `delete link` removes the file it points to; a link pointing out of the work dir is refused, as for any path.
- **Two meanings of a glob.** `grep`'s `glob` filter matches a name at any depth (`*.rs`), as in `rg`; `glob`'s pattern is a path from the searched directory (`*.rs` top level, `**/*.rs` anywhere), as in Claude Code's `Glob`.
- **Ignore rules come from the work dir down.** Rules in directories above it, a global gitignore, and the excludes of a git worktree (whose `.git` is a file pointing outside) are not read, since reading them would leave the work dir. A `.gitignore` applies without a `.git`.
- **Listings are sorted**, and a glob never brings back a file the ignore rules leave out.
- **Nothing is published** for checkpoints or diff views (`FilesChanged`, `FilesRead`): keelrs has no tool-call events yet.

## Known gaps

- **Two changes to one file in one reply can race**: calls of one reply run at once. [How a reply's tool calls are ordered](https://github.com/fpaschos/keelrs/issues/35).
- **A listing walks ignored directories** (`target/`, `.git/`) before dropping them. [Where grep and the listing run on a remote sandbox](https://github.com/fpaschos/keelrs/issues/66).
- **A case-only rename is refused on a case-insensitive filesystem** (macOS by default): `move Readme.md README.md` resolves both to one file and is refused as "the same file".
- **A dangling link cannot be removed**: `delete` follows it to its missing target and says "no such file".
