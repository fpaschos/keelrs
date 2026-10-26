#!/usr/bin/env bash
# PreToolUse (Bash git commit*): run the AGENTS.md checks in both workspaces
# and block the commit (exit 2) if any fails. Skips commits that stage nothing
# Rust-relevant; `-a`/`--all` or path commits always run the checks.

set -uo pipefail

command -v jq &>/dev/null || { echo "pre-commit-checks.sh: jq not found, skipping" >&2; exit 0; }

input=$(cat)
cmd=$(jq -r '.tool_input.command // empty' <<<"$input")
cwd=$(jq -r '.cwd // empty' <<<"$input")

# The settings `if` filter also lets through compound commands with no commit
# in them, so check for an actual `git [-C dir] commit` here.
grep -Eq '(^|[;&|(][[:space:]]*|[[:space:]])git([[:space:]]+-C[[:space:]]+[^[:space:]]+)?[[:space:]]+commit([[:space:]]|$)' <<<"$cmd" || exit 0

root=$(git -C "${cwd:-.}" rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0

relevant='(\.rs|Cargo\.(toml|lock)|deny\.toml|rust-toolchain\.toml|rustfmt\.toml|\.clippy\.toml)$'

# -a/--all, -i/-o, or a path argument means the commit itself stages files.
# Paths are spotted as words naming an existing file; over-matching only costs
# a check run.
stages_more=false
if grep -Eq -- '(^|[[:space:]])(-[a-zA-Z]*[aio][a-zA-Z]*|--all|--include|--only)([[:space:]]|$)' <<<"$cmd"; then
  stages_more=true
else
  set -f
  for word in $cmd; do
    word=${word//[\"\']/}
    if [[ "$word" =~ $relevant && -e "$word" ]]; then
      stages_more=true
      break
    fi
  done
  set +f
fi

if ! $stages_more && ! git diff --cached --name-only | grep -Eq "$relevant"; then
  exit 0
fi

export CARGO_TERM_COLOR=never
failed=()
log=$(mktemp)
trap 'rm -f "$log"' EXIT

run() {
  local label=$1; shift
  if ! "$@" >>"$log" 2>&1; then
    failed+=("$label")
  fi
}

for ws in root examples; do
  manifest=Cargo.toml
  [[ $ws == examples ]] && manifest=examples/Cargo.toml
  run "$ws: fmt" cargo fmt --all --check --manifest-path "$manifest" -- --color never
  run "$ws: clippy" cargo clippy --locked --workspace --all-targets --all-features --manifest-path "$manifest" -- -D warnings
  run "$ws: test" cargo test --locked --workspace --manifest-path "$manifest"
  if command -v cargo-deny &>/dev/null; then
    run "$ws: deny" cargo deny --manifest-path "$manifest" --offline check
  else
    echo "pre-commit-checks.sh: cargo-deny not found, skipping deny ($ws)" >&2
  fi
done

((${#failed[@]} == 0)) && exit 0

{
  echo "Commit blocked; checks failed: ${failed[*]}"
  if grep -q 'lock file .* needs to be updated but --locked was passed' "$log"; then
    echo "examples/Cargo.lock is stale: run 'cargo update --manifest-path examples/Cargo.toml -p keelrs', stage it, commit again."
  fi
  echo "--- output (last 80 lines) ---"
  tail -n 80 "$log"
} >&2
exit 2
