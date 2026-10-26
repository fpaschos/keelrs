#!/usr/bin/env bash
# PreToolUse (Edit|Write): keep Cargo files under cargo's control (AGENTS.md).
#   Cargo.lock -> deny; Cargo.toml -> ask the user.

set -uo pipefail

command -v jq &>/dev/null || exit 0

file=$(jq -r '.tool_input.file_path // empty')

case "$(basename -- "$file")" in
  Cargo.lock)
    echo "Cargo.lock is generated: use cargo update / cargo add / cargo generate-lockfile (AGENTS.md)." >&2
    exit 2
    ;;
  Cargo.toml)
    jq -n '{hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "ask",
      permissionDecisionReason: "Manifests change through cargo (AGENTS.md); approve only if no cargo command exists."
    }}'
    ;;
esac
exit 0
