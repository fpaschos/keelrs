---
name: prime
description: Prime agent with full codebase understanding. Use at the start of a session to orient on structure, conventions, and current git state.
---

# Prime: Load Project Context

## Objective

Build working understanding of this codebase. This orients you on structure, active state and key conventions.

## Process

### 1. Read Core Documentation

Read `AGENTS.md` in full (`CLAUDE.md` imports it) — behavioral rules and repo conventions. Follow the doc pointers it mandates.

### 2. Check Current State

Check recent commits:
!`git log -10 --oneline`

Check current branch and working tree status:
!`git status`

List any active worktrees:
!`git worktree list`

## Output Report

Concise summary (under 300 words):

### Modules

- List modules and purpose

### Key Files Seen

- Entry points, notable handlers or services

### Current State

- Active branch, recent commits, uncommitted work

**Bullets over prose.**
