# rig-tools research: grep engine, and what changed after 0.46.0-rc.1

Date: 2026-11-12. keelrs pins `rig-core`, `rig-agent`, `rig-tools` `=0.46.0-rc.1`.
Rig checkout: `/Users/fpaschos/Dev/Work/sapp/rig`, HEAD `748e4b9f8` (2026-11-11).

Rig points used:

- `v0.46.0-rc.1` = `6eff50a2f` (2026-10-23). The crates.io packages say so (`.cargo_vcs_info.json`: `sha1 6eff50a2f…`).
- `v0.46.0` = `6e039cd24` (2026-10-31), 35 commits after rc.1.
- HEAD `748e4b9f8`, 149 commits after `v0.46.0`. Nothing after `v0.46.0` is released.

## Summary and recommendations

1. **Upgrade the pins to `=0.46.0` now, and add serde_json's `preserve_order` to keelrs in the same change.**
   - keelrs compiles against 0.46.0 with no code change.
   - Clippy passes with `-D warnings`, and all tests pass: 70 unit, 111 integration, 5 doc. This was checked in a copy at `/tmp/keelrs-up/v046`. The repo was not changed.
   - One behaviour changes without a test catching it. Rig stopped forcing serde_json's `preserve_order` and `float_roundtrip` features (`a0efbf908`, #2934). keelrs enables neither itself.
   - So every `serde_json::Value` in keelrs becomes a sorted `BTreeMap`. Tool parameter schemas sent to the model, and any JSON keelrs stores, now list keys alphabetically. The probe below shows it.
   - Fix: `cargo add -p keelrs serde_json --features preserve_order`. Add `float_roundtrip` too if keelrs wants rc.1's float parsing. This is a manifest change, so ask first.
2. **Do not move `grep` to ripgrep's engine (`grep-searcher` + `grep-regex`) now.**
   - Rig states no reason for its choice anywhere.
   - keelrs would gain little. The bytes are already in memory when matching starts, so the searcher's mmap and streaming do not apply.
   - Its correctness gains are narrow: UTF-16, non-UTF-8 text and multi-line patterns. Smaller fixes on the existing `regex` crate cover the useful part.
   - Revisit this when grep moves into the backend for remote sandboxes ([#66](https://github.com/fpaschos/keelrs/issues/66)).
3. **Plan the next upgrade, past 0.46.0, as real work.** HEAD breaks keelrs in three places, all in rig-tools: `clip` is removed, `read_files` returns `Observation`, and `files` returns `Listed`. That is 6 compile errors in `builtin/edit.rs`, `builtin/grep.rs` and `sandbox/local.rs`. Each is a small fix. HEAD also makes keelrs's `dangling_target` workaround unnecessary.
4. **Worth adopting from HEAD**, cheapest first:
   - Refuse `timeout: 0` in `bash`. Today keelrs passes it on as a 0 s timeout.
   - Add rig's non-interactive env (`GIT_EDITOR`, `EDITOR`, `VISUAL`, `PAGER`, `GIT_PAGER`, `GH_PAGER`) to `local()`'s base env.
   - Use `excerpt` and `Excerpt::mark`, which say where a cut-off match or change is, in `grep` and `edit`.
   - Distinguish unreadable files from skipped ones in `grep`, through `Observation`.
   - The CRLF note in `read` comes for free once on HEAD.

## Q1. Why rig-tools' search does not use ripgrep's engine

### What rig says

Nothing. I searched:

- **Code and docs:** all `.rs`, `.md` and `.toml` files in the rig checkout for `grep-searcher`, `grep_searcher`, `grep-regex` and `ripgrep`. Only behaviour references come back. `search.rs:368` says `// Not text, as \`rg\` skips a binary file.` `workspace.rs:195` describes `Listing::Visible` "as ripgrep lists it".
- **Commit messages:** on all branches, for the same words. Three hits (`c9c928d4f`, `771728a1a`, `1335f9fcc`). All say "ripgrep's view" of a file listing, not a matching engine.
- **History:** `git log -S'grep-searcher'` and `-S'grep_searcher'` find nothing. No version of rig ever depended on either crate.
- **Project guides:** `DEVELOPING.md`, `AGENTS.md`, `CONTRIBUTING.md` and `crates/rig-tools/README.md`. They have no dependency rule that applies here. `CONTRIBUTING.md:52` keeps `rig-core` light, and companion crates "add first-party behavior with heavier dependencies". rig-tools is a companion crate.

### What the evidence shows (inference)

- **rig-tools uses ripgrep's walker, not its searcher.** `Cargo.toml` (HEAD) comments the `ignore` dependency with "`search` walks the tree as git ignores it". `regex` sits beside it, with no comment.
- **Search reads bytes through `Workspace`, not from the disk.**
  - `find` (`search.rs:322`) lists files, then reads them in batches of 100 with `Workspace::read_files` (`search.rs:349`). That call is "a workspace where each call costs a round trip, such as a remote" (`workspace.rs:67`).
  - Each file comes back whole, up to `MAX_FILE_BYTES` (16 MB). Search then needs a `String` (`String::from_utf8`), runs a whole-text prefilter (`whole_text`, `search.rs:429`), and runs the regex line by line.
  - Remote workspaces (`remote.rs`, over SSH) must work the same way. This fits a plain regex over bytes in memory.
  - It does not rule out `grep-searcher`. `Searcher::search_slice` searches a `&[u8]` already in memory (`grep-searcher-0.1.17/src/searcher/mod.rs:769`).
- **WASM is not the reason.** rig-tools depends on `libc`, `notify` and `ignore`, and it spawns processes. It is not a WASM crate.
- **Rig tuned the plain-regex path instead of switching engines.**
  - `8e5077567` (#2796, "measured performance pass") added the whole-text prefilter. Its message gives the numbers on 1,000 files × 200 lines.
  - No-match search went from 9.4–9.8 ms to 5.6–6.4 ms. Search where every file matches got about 3 % slower.
  - It also adds a `divan` bench (`benches/tools.rs`).
- **Output is line-shaped data.** `Hit { path, line, text }` holds the line as a `String`, and the excerpt logic needs `&str`. That fits UTF-8-only search.

Conclusion: no stated reason. The likely reasons are simplicity, and that `regex` plus `ignore` were already there. Search works on whole files that a `Workspace` hands over, so a streaming searcher buys little.

### Would `grep-searcher` make keelrs's `grep` better?

keelrs's `grep` (`keelrs/src/builtin/grep.rs`):

- It lists through `Sandbox::list`.
- It reads 64 files at a time with `Sandbox::read_files(batch, MAX_FILE_BYTES)` (`grep.rs:22`, `:109`).
- It skips a file that is not UTF-8 (`grep.rs:118`).
- It runs rig-tools' whole-text prefilter, copied with `ignore_case` (`grep.rs:202`).
- It splits into `lines()` and runs `is_match` per line (`grep.rs:128-130`).
- It adds context lines itself.

| Aspect | Today (regex, per line) | With `grep-searcher` + `grep-regex` | Verdict |
|---|---|---|---|
| CRLF | `str::lines` drops the `\r`, so `foo$` matches `foo\r\n`. The prefilter uses `.crlf(true)` and skips the prefilter when the file has a lone `\r` (`lone_cr`). | Same, with `RegexMatcherBuilder::crlf(true)` and `LineTerminator::crlf()`. | No gain. |
| Multi-line patterns | A pattern with `\n` never matches, because each line is matched alone. The same is true in rig-tools. | `SearcherBuilder::multi_line(true)` (`mod.rs:388`) matches across lines. | A feature gain. Nobody has asked for it yet. |
| Binary files | Not-UTF-8 files are skipped and named. A valid UTF-8 file with NUL bytes is searched. | `BinaryDetection::quit(b'\0')` (`mod.rs:79`) skips on NUL, as `rg` does. Off by default (`mod.rs:510`). | Small gain. |
| Encoding | UTF-16 and Latin-1 files are skipped. A log with one bad byte is skipped whole. | BOM sniffing is on by default, so UTF-16 is transcoded. Other bytes are searched "as if UTF-8" (`mod.rs:533-553`). | Real gain, narrow. `regex::bytes::Regex` with `from_utf8_lossy` output gets most of it with no new crate. |
| Performance | I/O dominates: the backend reads each whole file (≤16 MB, 64 at a time). The prefilter skips non-matching files in one regex pass. A matching file is split into a `Vec<&str>` and matched line by line, and every hit is counted, even past the 100 cap. | `search_slice` finds candidates over the whole buffer, then finds their lines. The mmap and streaming paths do not apply, because the bytes are already in a `Vec`. | Small CPU gain on matching files only. Not measured. The read through `Sandbox` stays the cost. |
| Output parity with rig-tools | keelrs's format already differs (context, `literal`, `ignore_case`, 100 matches, 500 characters, skipped files named). The prefilter and the UTF-8 rule are rig-tools'. | keelrs would drift further from rig-tools' code, so later fixes in `search.rs` would no longer copy across. | A cost. |
| Dependency cost | `regex` and `ignore` are already there. | New crates: `grep-searcher`, `grep-matcher`, `grep-regex`, `memmap2`. Already in keelrs's lock: `bstr`, `encoding_rs`, `encoding_rs_io`, `memchr`, `log`, `regex-automata`, `regex-syntax`, `aho-corasick` (manifests of `grep-searcher-0.1.17` and `grep-regex-0.1.14`). Licence `Unlicense OR MIT`, which passes `deny.toml` (MIT allowed). | Low, but AGENTS.md says to ask before adding a crate. |
| Code | About 60 lines of context grouping and line splitting. | A `Sink` impl (`matched`, `context`, `context_break`) of about the same size. | About even. |

**Recommendation: keep the `regex` engine.** If non-UTF-8 text matters, switch the line match to `regex::bytes::Regex`, show lines with `String::from_utf8_lossy`, and skip files with a NUL in the first 8 KB, as `rg` does. That needs no new crate. Revisit `grep-searcher` (or running `rg` in the sandbox) together with [#66](https://github.com/fpaschos/keelrs/issues/66). For a remote sandbox, the win is to search where the files are and not ship 16 MB files to the host. That is a backend decision, not an engine one.

## Q2. Changes since 0.46.0-rc.1

### Upgrade to 0.46.0

How this was checked:

- I copied the worktree to `/tmp/keelrs-up/v046`, set the three pins to `=0.46.0`, and ran `cargo update -p rig-core -p rig-agent -p rig-tools`.
- Then I ran `cargo check` / `clippy -D warnings` / `test --workspace` with all targets and features. All passed. The test counts match rc.1 (70 / 111 / 5).
- `examples/` has no direct rig dependency.

The commits the coordinator named:

| Commit | Change | Effect on keelrs at 0.46.0 |
|---|---|---|
| `a0efbf908` #2934 `fix(core)!: stop forcing serde_json features` | rig-core's serde_json goes from `["raw_value", "preserve_order", "float_roundtrip"]` to `["raw_value"]` (`git diff v0.46.0-rc.1 v0.46.0 -- Cargo.toml crates/rig-core/Cargo.toml`). Rig's own code now removes keys with `remove_preserving_order` (`tool/schema.rs`). | **Behaviour change.** `cargo tree -e features -i serde_json` shows `preserve_order` and `float_roundtrip` on at rc.1 and gone at 0.46.0. serde_json's `Map` becomes a `BTreeMap` (`serde_json-1.0.154/src/map.rs:38-41`). Probe: `args_schema` of `{pattern, path, glob, context}` gives properties in that order at rc.1, and `context, glob, path, pattern` at 0.46.0. This affects the model's view of every tool (parameter order) and any stored or evented JSON. `structured.rs:93` uses `Map::remove`, which was `swap_remove` under `preserve_order`. Fix: `cargo add -p keelrs serde_json --features preserve_order`. |
| `132432343` #2913 `read returns images, long lines can be read on` | `Read` returns `ToolOutput` with an image for PNG/JPEG/GIF/WebP. A cut line's note gives a `sed … \| cut -b` command. `MAX_LINE_CHARS` and `CUT_MARK` move to `lib.rs`. `clip` stays. | No change. keelrs calls `ReadArgs::show`, which passes `path: None`, so it gets no read-on command, and its output is byte-identical. The image path is only in `Read::call`, which keelrs does not use. |
| `8c18a7453` #2940 `a tool path starting with ~ is in the home directory` | `Local::join` maps a leading `~` to `$HOME`. `fs::shown` adds `./` to a `~` folder. | No change. keelrs calls `Local.canonical/stat/read/…` with paths it already confined, never `join` or `resolve`. A `~/x` from the model stays a folder named `~` in the work dir, which is right for a confined sandbox. |
| `8c0f91dac` #2898, `3cb43192a` #2897, `abbc8c0d1` #2896 | Refactors: shared corpus policies, provider lookups typed, reasoning-blob placement per wire. | Compiles. No keelrs test changes. keelrs's `agent::rig` (`AgentRun`, `RunSpec::new`, `prepare_request`, `Catalog::builtin`, `DynModel`, `driver::{Exchange, Model, Opening, Transport}`) builds unchanged. |
| `59c667527` #2942 `headless runs … leave no tool processes behind` | The rig launcher kills the process groups a dead agent left behind. | No effect. keelrs dropped rig-tools' `Launcher` in keelrs `f85a06d`. |
| `1d6739e21`, `8d6205e51` (in range) | `rig_core::tool::feedback::unknown_tool_feedback` is new. `RunSpec` and `PreparedRequest` gain `provider_tools`. `TurnPolicy` gains `hosted`. | Additive. keelrs builds `RunSpec::new()` (`agent/rig/run.rs:293`), not a struct literal, so it compiles. |
| `shell.rs` (in `132432343`) | Long lines in a cut output are shortened at 2000 characters. The `SILENT` hint now names `timeout_secs`. | keelrs's copied `SILENT` keeps the rc.1 wording. That is right for keelrs, whose argument is `timeout`. |

### Per tool and helper: released in 0.46.0 vs unreleased on HEAD

"keelrs today" = whether keelrs (on rc.1) already behaves like the change.

| keelrs part ← rig part | Released in 0.46.0 | Unreleased (after 0.46.0, HEAD) | keelrs today | Adopt? (work) |
|---|---|---|---|---|
| `grep` ← `search.rs` (prefilter copied; `clip`, `MAX_FILE_BYTES`) | `~` paths (`8c18a7453`). `Listing::from_options`. | `f8f6cf49d` #2965: (a) `excerpt` replaces `clip`, and a hit cut before its match says where the match is, with a `sed\|cut` command. (b) `read_files` → `Observation::{Found, Missing, Excluded, Failed}`, and search names unreadable paths (`Unread`) and counts skipped files. (c) "No matches in the files read." `125417d4d` #2995: search `MAX_MATCHES` stays 200. Prefilter unchanged. | Prefilter same. keelrs names up to 5 skipped files and lumps "over 16 MB, not UTF-8 or unreadable" together. No match location on cut lines. | **Must** on upgrade: replace `clip` with `excerpt(..).kept()` (1 line). **Should**: use `Observation` to split unreadable from skipped (about 15 lines in `local.rs` + `grep.rs`, plus a `SandboxBackend::read_files` contract change, which is a design decision). **Could**: `excerpt` + `mark` with `Source` for long lines. `shell_path` is `pub(crate)`, so pass the quoted relative path. |
| `read` ← `ReadArgs::show`, `Seen`, `MAX_*` | Images. Read-on command (only through `Read::call`). | `f8f6cf49d`: `show_lines` adds `[CRLF line endings; edit keeps them]` or a mixed-endings line (`read.rs` `lines_from` passes `Endings::of`). `125417d4d`: `Read::call` refuses `offset: 0`. | No CRLF note. `offset: 0` silently reads from line 1 (`lines_from` uses `first.max(1)`). | CRLF note comes free on upgrade, through `show`. Offset 0: optional (3 lines). Images: a feature gap (below). |
| `edit` ← `edit::apply`, `Replacement`, `Applied`; reply copied (`around`, `places`, `changed_at`); `stale` rewords `not_found` | None (`edit.rs` unchanged rc.1 → 0.46.0). | `f8f6cf49d`: `around` cuts long lines with `excerpt` and marks where the change is (`changed_spans`, BOM-aware). A loosely ambiguous match no longer suggests `replace_all` (inside `apply`, so automatic). CRLF detection moves to `Endings::of`, same rule. The `not_found` strings that `stale()` matches (`edit.rs:744-753`) are unchanged. | Copied reply = rc.1. `clip` used. | **Must**: `clip` → `excerpt` (1 line). **Could**: port `changed_spans` and `mark` (about 60 lines copied). |
| `write` ← `wrote` copied, `Seen` | None. | Only the `Root` refactor in `write.rs`. Text unchanged. | Same. | Nothing. |
| `delete` ← `delete.rs` rules | None. | `Root` refactor only. | Same. | Nothing. |
| `move` ← `move_file.rs` rules | None. | **Removed**: `2e7cc04fb` #3048 "remove the unused move tool and the checkpoint plugin". `38abf14e5` #3043 refused a move between hard links just before. | keelrs keeps its own `move`. It does not call rig code. | keelrs's `files.rs:68` comment names a file that no longer exists upstream. Consider the hard-link refusal: `move a b` where both are links to one inode. keelrs already refuses "the same file" after canonicalizing. Check that a hard link is caught. |
| `bash` ← `OutputLimit::tail`, `MAX_*`; `plain`, `SILENT`, `sent`, `Kept::redact` copied; `rig_core::redact::{text, find, Kind}` | Long lines shortened at 2000 characters with a note. Status prefixes become constants. New `SILENT` wording. | `633561329` #2990: the shell no longer redacts. Redaction is a plugin via `LineFilter`. `rig_core::redact` gains `Streamed` and `text_with`, loses `json_text` and `tool_result`, and `json` returns `bool`. `text`, `find` and `Kind::PrivateKey` stay. `f8f6cf49d`: `fatal:` and mid-line `error:` lines are kept from the cut middle. `125417d4d`: no 10 MB kill ("rig has no caps of its own"). `timeout_secs: 0` refused. | keelrs keeps the tail only (rc.1 rig already kept head + error lines + tail). keelrs keeps the 10 MB stop on purpose (`MAX_PRINTED`). `timeout: Some(0)` becomes `Duration::ZERO` (`bash.rs:63`). | **Should**: refuse `timeout: 0` (3 lines). **Could**: head + error lines + tail (`Kept`), and a spill file of the whole output. That changes the `bash` contract, so it is a design decision. `redact::text`/`find` still compile. `Streamed` could replace the `in_key` state only if keelrs redacts across chunks; it splits into lines first, so no need. |
| `local()` ← `Local` (`stat`, `read`, `write`, `write_if`, `remove`, `rename`, `files(.., Listing::All)`, `canonical`, `read_files`), `rig_core::tool::{content_hash, ContentHash}` | `text_error` gains an `InvalidData` message (keelrs has its own `fail`). | **Breaking**: `files` returns `Listed { files, failed }` (`f8f6cf49d`). `read_files` returns `Vec<Observation<Vec<u8>>>`, and `Observation` is not `Clone` (`local.rs:235,252,256`). `bfe6fca80` #2999: `canonical` follows dangling links "to where a write would create its file" (`workspace.rs:139-144`). `content_hash` unchanged. | keelrs adds `dangling_target` on top of `canonical` (`local.rs:431`). | **Must**: map `Listed.files`. Report or ignore `failed`; ignoring keeps today's behaviour. Map `Observation::Found` → `Some`, else `None` (about 10 lines). **Should**: then delete `dangling_target`, about 15 lines. Keep its tests. |
| `local()` exec ← rig-tools `Launcher` (dropped in keelrs `f85a06d`) | `59c667527`: launcher cleans up left groups. | `7e6102dd0` #2970: every child goes through `rig_tools::process`. `detach` starts a new session (`setsid`) with no controlling terminal and sets `NON_INTERACTIVE` (`process.rs:1218-1270`). | keelrs does the session, no-tty and `GIT_TERMINAL_PROMPT=0` itself (`local.rs:134`). | **Should**: add `GIT_EDITOR`/`GIT_SEQUENCE_EDITOR`/`EDITOR`/`VISUAL` (a failing editor) and `PAGER`/`GIT_PAGER`/`GH_PAGER=cat` to the base env (a few lines). It is a contract (the sandbox env), so propose it first. No reason to go back to `Launcher`. |
| `Listing` (keelrs's own `sandbox/listing.rs`) ← rig-tools `Listing` semantics | `Listing::from_options` (internal). | Watch fixes for ignore files (`cd6bb6e95`, `748e4b9f8`). Remote git listings (`f583c584a`). | Own implementation, with rules from the work dir down. | Nothing. Not used by keelrs. |
| `structured` ← "Rig 0.46.0-rc.1's plain schema formatting" (`structured.rs:146`) | `remove_preserving_order` (same result under the old features). | `fd40a962b` #3092: `args_schema` drops `null` only from properties that are not `required`, and keeps it inside `Vec<Option<T>>` items. | keelrs keeps all nulls. | Re-check whether keelrs still needs its copy after that change. A required nullable field now keeps `null` upstream. |
| `rig_core::tool::{ToolExecutionError, ToolErrorKind, FileChange, HunkLine}`, `feedback::arguments_refusal`, `rig_agent::run::*` | See the 0.46.0 table. | `decode_args` (integral floats), `feedback::stopped_call_note` (additive). `FileChange` docs only. | — | Nothing forced. HEAD `cargo check` of the lib showed no error outside rig-tools. Test targets were not compiled there, because the lib failed first. |

### HEAD compile check

Done in `/tmp/keelrs-up/head`, with `[patch.crates-io]` pointing to the checkout. `cargo check --all-targets` gives:

```
E0432 unresolved import `rig_tools::clip`       keelrs/src/builtin/edit.rs:8, grep.rs:5
E0599 no method `iter` on `rig_tools::workspace::Listed`   keelrs/src/sandbox/local.rs:235
E0308/E0277 `Observation<Vec<u8>>` vs `Option<Vec<u8>>`, not Clone   local.rs:252, :256
```

### rig-tools features keelrs's built-ins lack

- **`read` of images** (`132432343`, released). It returns `ToolOutput::content` with base64 image parts. keelrs's `Tool` output is a `String` today, so this needs a contract decision.
- **Where a cut line's interesting part is**, and a command to show it (`excerpt`/`mark`, `f8f6cf49d`). It is used for search hits, edit changes and shell error words.
- **Read-on command** for a cut line in `read` (0.46.0). It is not reachable through `ReadArgs::show`. keelrs would need its own copy of `read_on`.
- **`bash` keeps the head, the error lines from the middle, and the tail**, and spills the whole output to a file the model can `read` or `grep` (rig `Kept`, `Spill`).
- **Search `files` mode**: only the paths with a match, or every file for an empty pattern. keelrs's `glob` covers the second.
- **`list` with `depth`.** keelrs has `glob` only.
- **Unreadable directories and files named** in `list` and `search` (`Listed.failed`, `Unread`).
- **Non-interactive editor and pager env** (`NON_INTERACTIVE`).

Deliberate differences that should stay (`docs/builtin-tools.md`): confinement always on, `~` not expanded, the env allowlist, keelrs's own `Listing`, sorted listings, and the `bash` 10 MB stop.

## Sources

**keelrs** (worktree `scripted-model`, HEAD `9dabd7b`):

- `keelrs/src/builtin/{grep,edit,read,write,files,bash,mod,structured}.rs`, `keelrs/src/sandbox/local.rs`
- `docs/builtin-tools.md`, `docs/sandbox.md`, `docs/adr/0004-keelrs-drives-rigs-agentrun.md`
- commit `f85a06d`

**rig** at HEAD `748e4b9f8`:

- `crates/rig-tools/{Cargo.toml, README.md, src/search.rs, src/workspace.rs, src/lib.rs, src/read.rs, src/edit.rs, src/shell.rs, src/process.rs, src/list.rs}`
- `crates/rig-core/{Cargo.toml, src/redact.rs, src/tool/schema.rs, src/tool/feedback.rs}`
- `AGENTS.md`, `DEVELOPING.md`, `CONTRIBUTING.md`

**rig-tools 0.46.0-rc.1** from crates.io: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-tools-0.46.0-rc.1/`.

**Rig commits:**

- Released in 0.46.0: `6eff50a2f` (rc.1), `6e039cd24` (v0.46.0), `a0efbf908` #2934, `132432343` #2913, `8c18a7453` #2940, `8c0f91dac` #2898, `3cb43192a` #2897, `abbc8c0d1` #2896, `59c667527` #2942, `1d6739e21` #2894, `8d6205e51` #2886
- Unreleased: `f8f6cf49d` #2965, `7e6102dd0` #2970, `633561329` #2990, `125417d4d` #2995, `bfe6fca80` #2999, `38abf14e5` #3043, `2e7cc04fb` #3048, `f583c584a` #3041, `fd40a962b` #3092
- Older: `8e5077567` #2796, `1335f9fcc`

**ripgrep crates** (crates.io manifests and source, fetched with `cargo info`): `grep-searcher-0.1.17` (`Cargo.toml`, `src/searcher/mod.rs`), `grep-regex-0.1.14` (`Cargo.toml`).

**serde_json 1.0.154:** `src/map.rs:38-41`.

**Checks run outside the repo** (copies, no repo files changed):

- `/tmp/keelrs-up/rc1`: baseline tests and the schema-order probe.
- `/tmp/keelrs-up/v046`: pins `=0.46.0`; check, clippy, tests and the probe.
- `/tmp/keelrs-up/head`: patched to the rig checkout; check.
