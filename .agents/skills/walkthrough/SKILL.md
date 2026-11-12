---
name: walkthrough
description: "Walk the user through a change stop by stop in a living tour: explain, hunt smells, fix in small commits, keep tour and PR in sync."
disable-model-invocation: true
argument-hint: "PR number or branch; a tour link only to override the PR's"
---

A **walkthrough**: the user reads a change one **stop** at a time and sets the pace; you are the guide who explains, hunts and fixes. The change is the PR the argument names, else the current branch's PR, else the branch's diff against its merge-base. The **head** is its latest commit; everything you show points at it.

Wide sweeps (mapping the change, a hunt, checking links) go to **subagents**: fresh-context agents that read the files and return findings, so the code they read stays out of this conversation. Brief each one with the files and the question, never your conclusion, and verify what it returns before the user sees it. Without subagents, do the sweep inline. The user loop, the explanations and every edit stay with you.

## 1. Orient

Find the ticket the change builds, taking the first source that names one: the argument, the PR description (`Refs #n`, `Closes #n`), the commit messages, the branch name. Fetch it and the decisions that bind it: the ADRs, the glossary, the resolution the ticket links. Done when the ticket, its decisions, the PR and the head SHA are named.

## 2. Tour

The **tour** is one page the user reads beside the code. Find it, then build it or bring it to the head, following [`reference/TOUR.md`](reference/TOUR.md). Done when every changed file belongs to a stop and every link resolves at the head.

## 3. Loop

Ask which stop the user is on. Then take each message as one of three kinds:

- **Question** (why, how, what does this do): explain from the code, citing `file:line`. A question is never a decision: answer with the explanation alone.
- **Smell** (something in the code looks duplicated, over-guarded, misplaced or out of line with its siblings): **hunt** that smell across the whole change, following [`reference/HUNT.md`](reference/HUNT.md). Report the hits, the options and your recommendation, then ask one question and wait for the answer before editing.
- **Decision**: build it, keeping behaviour unless the user asked to change it. One logical change per commit: run the `AGENTS.md` checks, report its metrics before and after, commit with the repo's commit skill and push. Tests follow the design: one test per behaviour, at the seam that reaches it, with no coverage lost.

A finding that belongs to a later ticket goes to that ticket once the user agrees, its body rewritten in place.

## 4. Sync

After each batch of commits, bring the tour to the new head ([`reference/TOUR.md`](reference/TOUR.md), "Sync"), then the PR description. Done when both describe the head.

The walkthrough ends when the user says so. Report the commits, the tour link, and any ticket changed.
