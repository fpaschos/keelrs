# Agents run on the host; a sandbox confines only what tools do

keelrs runs every agent on the host, as many concurrent actors, and gives them sandboxes only for what their sandboxed tools do: reading files and running commands. An agent never runs inside a sandbox. The alternative, starting a coding-agent process inside each container, puts the model's credentials and every secret a tool needs inside the box the model's commands can read, and costs one heavy process per agent. On the host, model tokens and secrets stay with the agent and its host tools (a credential proxy returns only results), many agents share few sandboxes, and the sandbox can be swapped (this machine, a container, a microVM, a remote host) without touching the agent.

## Consequences

- Isolation is the sandbox's: host tools are trusted code, and keelrs cannot stop one from touching the host.
- keelrs's own code reaches the host's files and processes only in `sandbox::local`, and `keelrs/clippy.toml` enforces it; an application can opt its own tools in to the same rule.
- Commands in a local sandbox start from an allowlist of host variables, never the whole environment, so secrets in the host's env stay with the agent.
- Network egress is controlled later by proxies, one inside the sandbox for commands and one in the agent for host tools, not by moving the agent.
