#!/usr/bin/env bash
# Skill extensions: add this repo's rules for a skill to its context when it runs.
#   UserPromptExpansion: a skill the user types (/wayfinder)
#   PostToolUse on Skill: a skill the model starts (wayfinder -> grilling)
# For skill <s>, adds docs/agents/skills/<s>.md if it exists. Never blocks;
# Claude Code records a failed run in the session transcript.

set -uo pipefail

command -v jq &>/dev/null || exit 0

IFS=$'\t' read -r event skill < <(jq -r \
  '[.hook_event_name, (.tool_input.skill // .command_name // "")] | @tsv')
skill=${skill##*:} # a plugin skill is named <plugin>:<skill>
[[ $skill =~ ^[A-Za-z0-9_-]+$ ]] || exit 0

doc="docs/agents/skills/$skill.md"
[[ -f "$CLAUDE_PROJECT_DIR/$doc" ]] || exit 0

jq -n --arg e "$event" --arg p "$doc" --rawfile d "$CLAUDE_PROJECT_DIR/$doc" \
  '{hookSpecificOutput: {hookEventName: $e, additionalContext: ("Repo rules for this skill (" + $p + "):\n\n" + $d)}}'
exit 0
