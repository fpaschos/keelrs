# Every agent declares exactly one sandbox per render

Every agent function calls `use_sandbox` exactly once per render, as it calls `use_model`: no call, or a second one, settles the dispatch as Failed, and there is no default sandbox (`sandbox::local()` is the one to reach for when there is nothing else). This reverses [#40](https://github.com/fpaschos/keelrs/issues/40) decision 7 (optional, at most once) and [#42](https://github.com/fpaschos/keelrs/issues/42) decision 3 (no sandbox, no built-ins). The alternative, an optional sandbox, keeps sandbox-free agents a few lines shorter, but every layer then carries the "no sandbox" case: a `sandboxed` flag on each tool, a check that a sandboxed tool has a sandbox, a panic behind that check, built-ins that appear and disappear, and a fallback in every plan that reads through the sandbox (AGENTS.md, skills). With one sandbox always present, that case does not exist.

## Consequences

- Agents that use no files declare a sandbox anyway, and narrow the built-ins with `.only([])` when they want no tools; their tool definitions are otherwise sent on every request, as in other harnesses.
- The declarations every render must make are checked first, model then sandbox, so a missing one fails before tools, skills or the model connection are looked at.
- The sandbox may still change between renders, but an agent instance expects to keep one: its built-in tools' cache (`Seen`) is a single slot for the current sandbox id, emptied when the id changes, never one per sandbox. A narrowed work dir keeps its sandbox's id, and so its cache.
- An application that moves an agent to another sandbox for good starts a new agent instance.
