#!/usr/bin/env bash
# PostToolUse (Edit|Write on *.rs): format the edited file.
# The root rustfmt.toml sets the edition, so bare rustfmt agrees with cargo fmt.
# Never blocks: missing tools or rustfmt errors are reported and ignored.

set -uo pipefail

for tool in jq rustfmt; do
  if ! command -v "$tool" &>/dev/null; then
    echo "format.sh: $tool not found, skipping" >&2
    exit 0
  fi
done

file=$(jq -r '.tool_input.file_path // empty')
[[ -n "$file" && -f "$file" ]] || exit 0

rustfmt "$file" >&2 || echo "format.sh: rustfmt failed on $file" >&2
exit 0
