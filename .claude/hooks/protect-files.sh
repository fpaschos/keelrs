#!/usr/bin/env bash
# PreToolUse (Edit|Write, Bash): keep Cargo files under cargo's control (AGENTS.md).
#   Cargo.lock -> deny; Cargo.toml -> ask the user.
# For Bash, catches in-place edits and redirects (sed -i, perl -i, >, >>, tee);
# a script that writes the file itself is not caught.

set -uo pipefail

command -v jq &>/dev/null || exit 0

input=$(cat)
tool=$(jq -r '.tool_name // empty' <<<"$input")

deny_lock() {
  echo "Cargo.lock is generated: use cargo update / cargo add / cargo generate-lockfile (AGENTS.md)." >&2
  exit 2
}

ask_toml() {
  jq -n '{hookSpecificOutput: {
    hookEventName: "PreToolUse",
    permissionDecision: "ask",
    permissionDecisionReason: "Manifests change through cargo (AGENTS.md); approve only if no cargo command exists."
  }}'
  exit 0
}

if [[ $tool == Bash ]]; then
  cmd=$(jq -r '.tool_input.command // empty' <<<"$input")
  # One pattern per write form; [^;&|]* keeps the match inside one command.
  writes='((sed|perl)[[:space:]][^;&|]*(-[a-zA-Z]*i|--in-place)[^;&|]*|(>|>>)[[:space:]]*|tee[[:space:]]+(-a[[:space:]]+)?)[^;&|[:space:]]*Cargo\.'
  grep -Eq "${writes}lock" <<<"$cmd" && deny_lock
  grep -Eq "${writes}toml" <<<"$cmd" && ask_toml
  exit 0
fi

file=$(jq -r '.tool_input.file_path // empty' <<<"$input")
case "$(basename -- "$file")" in
  Cargo.lock) deny_lock ;;
  Cargo.toml) ask_toml ;;
esac
exit 0
