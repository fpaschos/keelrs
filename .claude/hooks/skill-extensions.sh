#!/usr/bin/env bash
# Skill extensions: add this repo's rules for a skill to its context when it runs.
#   UserPromptExpansion: a skill the user types (/wayfinder)
#   PostToolUse on Skill: a skill the model starts (wayfinder -> grilling)
# For skill <s>, adds docs/agents/skills/<s>.md if it exists, and logs the run
# to skill-extensions.log. Never blocks.

set -uo pipefail

command -v jq &>/dev/null || exit 0

input=$(cat)
event=$(jq -r '.hook_event_name // empty' <<<"$input")
if [[ $event == PostToolUse ]]; then
  skill=$(jq -r '.tool_input.skill // empty' <<<"$input")
else
  skill=$(jq -r '.command_name // empty' <<<"$input")
fi
skill=${skill##*:} # a plugin skill is named <plugin>:<skill>
[[ $skill =~ ^[A-Za-z0-9_-]+$ ]] || exit 0

root=${CLAUDE_PROJECT_DIR:-$(jq -r '.cwd // "."' <<<"$input")}
doc="docs/agents/skills/$skill.md"
[[ -f "$root/$doc" ]] || exit 0

printf '%s %s %s %s %s\n' "$(date -u +%FT%TZ)" "$(jq -r '.session_id // "-"' <<<"$input")" \
  "$event" "$skill" "$doc" >>"$root/.claude/hooks/skill-extensions.log"

jq -n --arg e "$event" --rawfile d "$root/$doc" --arg p "$doc" \
  '{hookSpecificOutput: {hookEventName: $e,
    additionalContext: ("Repo rules for this skill (" + $p + "); they add to the skill, and where the two disagree, these win:\n\n" + $d)}}'
exit 0
