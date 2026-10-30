# keelrs drives Rig's `AgentRun`; its own events are the source of truth

keelrs sits on Rig by driving `rig_agent::run::AgentRun` itself (pinned to the latest Rig RC). It does not use Rig's `AgentRunner` with hooks, and it does not write its own loop over `rig_core`'s models. The agent function runs again before every turn and declares that turn's model, prompt and tools, so keelrs must own the turn boundary. `AgentRunner` only reaches it through per-turn patches, models registered in advance, and stop-and-restart. Driving `AgentRun` gives keelrs the boundary while Rig keeps tool-call validation and recovery, output policy, turn budget and stream assembly. An own loop gives the same boundary, but keelrs would write all of that itself. Three rules keep the choice reversible:

1. keelrs's canonical events (Flue-style) are the source of truth. A run's messages are computed from them, an `AgentRun` is never saved (its JSON is tied to one Rig version), and the events record what `AgentRun` accepted, not what the model sent.
2. Rig types stay inside private modules (`agent::rig` for the loop, `sandbox::local` for the local sandbox on `rig-tools`, `builtin` for the built-in tools on `rig-tools`' text logic) and never appear in keelrs's public API. If `AgentRun` falls short, an own loop over `DynModel` replaces it inside `agent::rig`; if `rig-tools` does, `sandbox::local` is rewritten behind the same `SandboxBackend`, and `builtin`'s helpers are copied in. `builtin` acts only through `Sandbox`, never a Rig `Workspace`.
3. `AgentRun` has no steering and no writes to a live run's history. So steering, history edits and resume build a new `AgentRun` from the events at a `CallModel` step; a live run is never patched.

## Consequences

- Rig's agent hooks do not run; keelrs emits its own events around each `AgentRun` step.
- Rig counts usage and turn budget per `AgentRun`, so keelrs adds them up across the rebuilds rule 3 causes.
