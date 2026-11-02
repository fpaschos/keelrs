# Archon's workflows as flow ideas

Which flows Archon ships, the patterns they repeat, how Archon tests them, and which ones keelrs could copy as
examples once its imperative flow API exists. Source: Archon `3166b6176` (`../archon`), with workflows under
`.archon/workflows/` and the language reference in `packages/docs-web/src/content/docs/reference/workflow-language.md`
("docs" below). Facts come from reading the code and were not re-checked line by line. Inferences are marked.
Worktree and container isolation are deliberately left out: keelrs does them by hand for now.

## Composition

- `include:` inlines another workflow when it loads. The included node ids get a prefix (`attend__ask`).
- `workflow:` starts a separate child run, with its own record, cost and artifacts (docs:85-100, :1991).

## The workflows

Paths are relative to `.archon/workflows/`.

| Workflow | Purpose | Shape | Calls |
|---|---|---|---|
| examples/proactive-issue-investigation | A small model classifies an issue, a stronger one investigates and drafts a reply, a person approves, a bash node posts it | linear DAG with `when` routing, an approval wait (:56-70), a `cancel` node, triggered when an issue opens | none |
| maintainer/maintainer-review-pr | Deep review of one PR | classify, then 5 lenses each gated by `when`, then synthesize (`one_success`, :171-175), post (bash), record state (script, :207), report | none |
| maintainer/maintainer-standup | Daily brief | 3 parallel gather scripts, then synthesize, then persist (:25-60) | none |
| sdlc/intake | Claims a waiting item and checks whether the author is a maintainer (script) before any model runs | facts, conditional triage, outcome; triggered by an event | include triage-block (:93) |
| sdlc/triage-entry → triage/triage-block | Grounds an item and routes it to investigate, plan, deliver, continue or no-action | the entry is a `loop_group` cycle with an attention pause; the block is policy, then triage, then a verdict script | include triage-block, attention |
| sdlc/plan-entry → plan/plan-block | Turns decided intent into a plan | same entry/block split as triage | include plan-block, attention |
| sdlc/ship-entry → ship/ship-block | Takes the least speculative path to delivery | forge, triage, then a 3-way branch joined by deliver (`none_failed_min_one_success`, :147-150) | include triage-block, investigate, plan-block, deliver-block (:71-130) |
| sdlc/deliver-entry → deliver/deliver-block | Implement, open the PR, review, simplify, CI | a `loop_group` with `max_iterations: 5`, ending when `until_bash` reads `$round.output.done` (:81-89) | include implement, pr, review-block, simplify (:92-209) |
| sdlc/implement | Implement until checks pass | one `loop:` node with `until_field: done`, max 5, `fresh_context` (:33-38) | none |
| sdlc/review-entry → review/review-block | Parallel review lenses, then one verdict | scope, mode, route (small model), 5 lenses, an `all_done` barrier, synthesize, publish, prune | none |
| sdlc/simplify | Read-only judgment of structure | prompt, then a cleanup script | none |
| sdlc/investigate | Root-cause report | command, then a verdict script | none |
| sdlc/validate | Find and run the project's checks | discover (AI), run (script, `always_run`, `on_timeout: skip`, :59-66), classify failures, result | none |
| sdlc/pr | Open or push a PR | forge check, prepare (AI), publish (script) | none |
| sdlc/upkeep | Bump one dependency | forge, assess, conditional deliver, outcome | include deliver-block (:83) |
| sdlc/build-queue | Claims a triaged item and drives it to a ready PR | facts, route, checkout, ship, answer, apply, ask, ruling; started by a trigger (:13-14) | include ship-block (:191) |
| sdlc/merge-queue | Merges finished PRs through a gate script | facts, sweep agent, record, retriage, ask, ruling; `mutates_checkout: false` | none |
| sdlc/discoveries | Works through out-of-scope discoveries recorded by earlier runs | facts, decide, outcome, ask, answer | none |
| sdlc/attention | Shared "decide, then ask a person" block | answer script, decide (`when`), settle (`all_done`), ask wait | none |
| sdlc/metrics-test-value | Judges each PR's tests | script, then a `workflow:` fan-out (`items: $prs.output.urls`, `max_parallel: 3`, :42-48), then scoring | metrics-test-value-judge |
| sdlc/*-judge | Model as judge | read script, then a judge prompt with a schema, `allowed_tools: []` | none |
| sdlc/metrics-prs, forge-check, cleanup | Deterministic | one script node | none |

## Patterns

- **Facts before the model.** A script gathers state and picks a `path`. A model runs only on the paths that need one,
  gated by `when: "$facts.output.path == 'route'"` (build-queue:51-99, merge-queue:48-70).
- **Classify, then route.** A cheap classifier returns a value from an enum schema, and `when` branches on it
  (proactive:19-39). The branches can rejoin (ship-block).
- **Parallel lenses, then synthesize.**
  - A router turns each lens on or off.
  - Every lens runs with `context: fresh`.
  - An `all_done` barrier waits for them, then a synthesizer combines their findings (review-block:191-301).
- **A bounded loop with a computed exit.**
  - A `loop_group` runs until a script reports `done`. The round script stops one round before `max_iterations`, so
    that hitting the cap ends with a typed result rather than an error (deliver-block:86-87).
  - Each round reads the previous round's summary through `$LOOP_PREV.round.output.summary`.
- **Entry and block.**
  - A block is a pure DAG.
  - Its entry wraps it in a `loop_group`: run the block, ask a person, run it again if they answer `rerun`.
  - The person's answer feeds into the next pass (deliver-entry:51-80).
- **A person's decision as a durable pause.** `wait: {attention, form: approval|select|multi_select|message}`
  (docs:683-780). The question shows what happened, why it matters, a recommendation with its confidence, the choices,
  and what not answering costs (deliver-entry `round-cap.stubs.yaml`:56-69).
- **Code acts on the decision.** After approval, a bash node posts the text, not an agent, so the approved text cannot
  be reworded (proactive:72-80). The merge queue merges only through a gate script.
- **Queues with claims.** A work item is a state document, claimed by a conditional write (`--if-revision`). The claim
  is released only when the run that owns it ends, never because it is old (`sdlc/.shared/state.ts:4-6`,
  `workitem.ts:8-10`).
- **State between runs.** `archon state` documents carry revisions (docs:1417), alongside `$STATE_DIR` (docs:1351).
- **Typed output everywhere.**
  - `output_format` is a JSON Schema, and a node that declares one but returns nothing valid fails (docs:581).
  - Scripts certify their own stdout the same way.
- **Join rules.** `trigger_rule` accepts `all_success`, `one_success`, `none_failed_min_one_success` and `all_done`
  (docs:318). Cleanup and report nodes use `all_done`.
- **Fan-out over a list known only at runtime.** `fan_out` on a `workflow:` node (docs:2238-2285) returns an array in
  item order. Its join defaults to `all_done`.

## Testing with stubbed nodes

- `archon workflow test <name>` replays `fixtures/*.stubs.yaml`. `--dry-run --stubs <file>` replays one file
  (docs:2989-2995). No model provider is called.
- Each top-level key is a node id, with a prefix for included nodes (`review__synthesize`). Its value is the node's
  output: an object for a node with a schema, a string otherwise.
- Scripts run for real when the fixture sets `exec-code: true`.
- A wait's answer is stubbed like any other output.
- Reaching a node that has no stub fails the fixture.
- The `fixture:` block asserts `expect: completed|failed|paused|cancelled`, `fail-node`, `reached: [...]`,
  `resolved-text-contains`, and `inputs` (`packages/workflows/src/fixture-runner.ts:9, 72-84`).

```yaml
classify: { route: investigate, reason: "stub: ..." }
investigate: { comment: "stub: ..." }
review: { decision: reject, text: "wrong file" }
fixture: { expect: cancelled, reached: [review, drop], inputs: { repository: acme/widgets, issue_number: "44" } }
```

## Node kinds and options

- **Node kinds** (`packages/workflows/src/schemas/dag-node.ts:164-190`):
  - work: `command`, `prompt`, `bash`, `script`;
  - control: `loop`, `loop_group`, `wait`, `cancel`;
  - composition: `include`, `workflow` (with `isolation` and `fan_out`).
- **Common options** (`schemas/dag-node-base.ts:67-124`):
  - wiring: `depends_on`, `when`, `trigger_rule`;
  - model and context: `model`, `provider`, `context` (`fresh|shared|{resume}`), `effort`;
  - output and tools: `output_format`, `allowed_tools`, `denied_tools`;
  - limits: `idle_timeout`, `retry`, `maxBudgetUsd`, `always_run`.
- **Models.** `model` may be a list (review-block:216), probably tried in order (inference). Models are also named by
  tier: small, medium or large.
- **Loops** (`schemas/loop.ts:64-97`): `until`, `until_bash`, `until_field`, a required `max_iterations`, and
  `fresh_context`.
- **Retry** (`schemas/retry.ts:10-27`): `max_attempts` from 1 to 5, a delay, and `on_error: transient|all`.
  - AI nodes retry transient errors by default.
  - Bash and script nodes retry only if they opt in.

## What keelrs could copy

Copy these as examples after this map's core ergonomics land. Each one forces a primitive onto the Rust API.

1. **Investigate an issue** (proactive-issue-investigation): classify, branch, wait for approval, then post with code.
   It forces typed results, branching on them, and a durable pause for a person's decision. Its three fixtures are
   ready-made test cases.
2. **Implement↔review rounds** (deliver-block): a bounded loop whose exit code computes, the previous round's state
   passed forward, fresh context each round, and a cap that ends in a typed result. This is the next
   `build-review-loop`.
3. **Parallel lenses, then synthesize** (review-block): concurrent fan-out with model and tools set per branch, a
   router, a choice of join rule, and cleanup that always runs.
4. **Entry, block and a person's decision**: sub-flows as functions that return a typed outcome, a reusable "decide,
   then ask a person" piece, and re-entry with the answer.
5. **Claim, act, record** (build-queue, merge-queue): a persistent state store with compare-and-set claims, a facts
   step before any model spend, and a gate the agent cannot bypass. A simpler variant is a fan-out over a runtime list
   with `max_parallel` (metrics-test-value).

All five would need a stub-and-replay harness: canned output per node id, with assertions on the final status, the
nodes reached and the output text (inference: this is what lets a flow be tested without calling a model).
