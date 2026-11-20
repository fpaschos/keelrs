#!/usr/bin/env bash
# Skill extensions: add this repo's rules to a skill's context when it runs.
#   UserPromptExpansion: a skill the user types (/wayfinder #43)
#   PostToolUse on Skill: a skill the model starts (wayfinder -> grilling)
# For skill <s>, adds docs/agents/skills/<s>.md (always) and every
# docs/agents/skills/<s>/*.md whose optional first line `args: <regex>`
# matches the skill's arguments. Logs each run to skill-extensions.log.
# Never blocks.

set -uo pipefail

command -v jq &>/dev/null || exit 0

input=$(cat)
event=$(jq -r '.hook_event_name // empty' <<<"$input")
if [[ $event == PostToolUse ]]; then
  skill=$(jq -r '.tool_input.skill // empty' <<<"$input")
  args=$(jq -r '.tool_input.args // empty' <<<"$input")
else
  skill=$(jq -r '.command_name // empty' <<<"$input")
  args=$(jq -r '.command_args // empty' <<<"$input")
fi
skill=${skill##*:} # a plugin skill is named <plugin>:<skill>
[[ -n $skill && $skill =~ ^[A-Za-z0-9_-]+$ ]] || exit 0

root=${CLAUDE_PROJECT_DIR:-$(jq -r '.cwd // "."' <<<"$input")}
dir="$root/docs/agents/skills"

files=()
[[ -f "$dir/$skill.md" ]] && files+=("$dir/$skill.md")
for f in "$dir/$skill"/*.md; do
  [[ -f $f ]] || continue
  first=$(head -n 1 "$f")
  if [[ $first =~ ^args:[[:space:]]*(.+)$ ]]; then
    grep -Eq -- "${BASH_REMATCH[1]}" <<<"$args" || continue
  fi
  files+=("$f")
done
((${#files[@]})) || exit 0

context="Repo rules for /$skill (they add to the skill; where the two disagree, these win):"
for f in "${files[@]}"; do
  context+=$'\n\n'"<!-- ${f#"$root"/} -->"$'\n'"$(grep -Ev '^args:' <<<"$(head -n 1 "$f")"; tail -n +2 "$f")"
done

printf '%s %s %s %s args=%q files=%s\n' \
  "$(date -u +%FT%TZ)" "$(jq -r '.session_id // "-"' <<<"$input")" "$event" "$skill" "$args" \
  "$(printf '%s,' "${files[@]#"$root"/}")" >>"$root/.claude/hooks/skill-extensions.log"

jq -n --arg e "$event" --arg c "$context" \
  '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}}'
exit 0
