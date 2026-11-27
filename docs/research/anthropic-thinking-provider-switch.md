# Anthropic thinking after a provider switch inside a tool loop

Research for [#115](https://github.com/fpaschos/keelrs/issues/115) (part of #43).

The case: a run picks the model per turn. The last assistant turn in the
history has tool calls and came from OpenAI or Gemini, so it has no signed
Anthropic thinking block. The next turn goes to Anthropic with thinking on.
Does Rig `0.46.0-rc.1` send a request Anthropic accepts?

Sources and pins:

- Anthropic docs, read 2026-11-27: [Thinking](https://platform.claude.com/docs/en/build-with-claude/thinking),
  [Extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking),
  [Troubleshooting thinking](https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting),
  [API errors](https://platform.claude.com/docs/en/api/errors).
- Rig `v0.46.0-rc.1` = `6eff50a2fd95de3a505000a0e39d2893d83c6f4d` (keelrs's pin).
  Rig `v0.47.1` = `f4d73dacdfabb3d9daa3f1de091efa490a84c601`. Rig `main` =
  `8b9337da6e05b42fd7db022f0d5711094d80bc5e` (v0.47.1 plus a README commit).
- pi `main` = `2bd2c2d0a6a13e9a2d760073bd6e67b662964a60` (2026-11-03); pi `v1.1.0` = `abe508e1b89912adde45528136c3221eb69acdd7`.
- Flue `main` = `deee93b40d275ecc9a50c50af2158f20089d54bc` (2026-10-12). Flue uses `@earendil-works/pi-ai` `^1.1.0`, locked at `1.1.0`.

Rig paths below are under `crates/rig-core/src/`, at `v0.46.0-rc.1` unless
another tag is named.

## Short answer

1. **Anthropic's rule.** The "final assistant turn must start with a thinking
   block" rule holds only in manual mode (`thinking.type: "enabled"`).
   Adaptive mode drops it: "no assistant turn needs to start with one". You
   cannot turn thinking on or off inside a tool loop. If you do, the API does
   not fail. It turns thinking off for that request. Signed blocks
   (`thinking`, `redacted_thinking`) in the latest assistant turn must go back
   unchanged. The API checks their signatures. A block from a model that
   the target cannot read is dropped without an error. Many current models
   cannot turn thinking off at all (`"disabled"` gets a 400).
2. **What Rig does.** `adapt` sees that the turn's `Origin` is not the
   target model. It keeps only canonical fields. It clears provider items.
   Reasoning becomes plain text, because Anthropic says
   `foreign_reasoning: false`. Redacted or empty reasoning is dropped. Call
   ids are normalized to `[a-zA-Z0-9_-]{1,64}`. The encoder sends that turn
   as `[text?, tool_use…]` with no `thinking` block. Rig does not turn
   thinking off and does not add a thinking block. The `thinking` field is
   what the caller mapped (keelrs: `adaptive` plus `output_config.effort`).
   For Claude models that bind thinking to context, Rig also adds
   `block_binding: drop_block`.
3. **Is it accepted?** Yes, for what keelrs sends. keelrs maps every
   `Thinking` level to a Rig `Effort` (or `Off`), never to a `Budget`, so
   Rig sends `adaptive`, and adaptive has no "starts with thinking" rule.
   Only a caller that forces `{"type": "enabled", "budget_tokens": N}` hits
   the manual-mode rule. The current docs say the API then turns thinking
   off for that request ("degrade gracefully"), not a 400. No live test
   in Rig, pi or Flue covers this exact case. The docs list no error
   message for it.
4. **What Flue (pi-ai) and Rig do.** Both use the same plan: compare the
   turn's origin (api, provider, model) with the target. If the target is a
   different model, remove signatures and provider data, and keep text and
   tool calls. They differ on the reasoning text. Rig (and pi `1.1.0`,
   which Flue locks) send it as plain text. pi since `v1.3.0` (commit
   `911309429`, 2026-10-15) drops it, because the new model read it as its
   own visible reply. Neither turns thinking off or inserts a thinking block.
   Neither needs to in adaptive mode.

**What keelrs's canonical message must keep.** keelrs stores Rig's
`Message` as is (`keelrs/src/agent/rig/history.rs:18`), so it already keeps
all of these. Keep it that way:

- The turn's `Origin`: `api`, `provider`, `model` (the requested model), and
  `context` (the tools and system fingerprint). Rig's sameness rule reads all
  of them. Without an origin, a turn is always foreign.
- Each block's provider item (`Native`) verbatim: the Anthropic `signature`,
  the `redacted_thinking` `data`, OpenAI encrypted reasoning, Gemini
  `thoughtSignature`. The same model needs them to continue its reasoning.
  Another model needs them to be removed.
- Reasoning as its own block (not merged into text), with a `redacted` flag.
  This lets the converter drop or convert it per target.
- The tool call id as the provider wrote it. Rig normalizes it per target.
- The stop reason. Rig drops failed turns and handles aborted turns on its
  own.

## Evidence

### 1. Anthropic's rules

Manual mode and adaptive mode differ. From
[Thinking: Thinking with tool use](https://platform.claude.com/docs/en/build-with-claude/thinking#thinking-with-tool-use):

> The entire turn runs in a single thinking mode: you can’t toggle thinking
> in the middle of a turn, including during the tool-use loop. In extended
> (manual) mode, the API additionally enforces that the final assistant turn
> of a thinking-enabled request starts with a thinking block. Adaptive mode
> relaxes this: no assistant turn needs to start with one.

> **Mid-turn conflicts degrade gracefully.** If you toggle thinking mid-turn
> (for example, between sending a tool call and returning its result), the
> API doesn’t error. Instead, it silently disables thinking for that request.
> To preserve model quality, the API might strip thinking blocks that would
> create an invalid turn structure, or disable thinking when the conversation
> history is incompatible with thinking being enabled. To confirm whether
> thinking was active, check for the presence of `thinking` blocks in the
> response.

From [Extended thinking: Turn structure in manual mode](https://platform.claude.com/docs/en/build-with-claude/extended-thinking#turn-structure-in-manual-mode):

> Manual mode adds one requirement: the final assistant turn of a
> thinking-enabled request must start with a thinking block ([adaptive
> thinking] drops that requirement).

Manual mode is legacy: "Claude 4.7 and later models do not support it and
reject requests that use it, returning a 400 error" (same page).

Signed blocks go back unchanged. From
[Preserving thinking blocks](https://platform.claude.com/docs/en/build-with-claude/thinking#preserving-thinking-blocks):

> * **Required:** within a tool-use turn, pass thinking blocks back.

> Within the latest assistant message, the sequence of consecutive
> `thinking` blocks must match what the model generated in the original
> request: you can’t rearrange, edit, or partially drop them. This includes
> [`redacted_thinking` blocks].

From [Thinking encryption](https://platform.claude.com/docs/en/build-with-claude/thinking#thinking-encryption):

> Full thinking content is encrypted and returned in the `signature` field on
> each thinking block. The API uses the signature to verify that thinking
> blocks were generated by Claude when you pass them back.

> * The `signature` field is opaque: don’t interpret or parse it.
> * `signature` values are compatible across platforms: a value generated on
>   one platform works on another.

From [Redacted thinking blocks](https://platform.claude.com/docs/en/build-with-claude/thinking#redacted-thinking-blocks):

> The `data` field is opaque and encrypted. As with the `signature` field on
> regular thinking blocks, pass `redacted_thinking` blocks back to the API
> unchanged when continuing a multi-turn conversation with [tools].

The errors for changed blocks ([API errors](https://platform.claude.com/docs/en/api/errors)):

> `thinking` or `redacted_thinking` blocks in the latest assistant message
> cannot be modified. These blocks must remain as they were in the original
> response.

> messages.{i}.content.{j}: Invalid `signature` in `thinking` block. The block
> is bound to a different conversation. Remove the block, or set
> `thinking.block_binding.prefix_mismatch_behavior` to "drop_block".

Signatures from other models. From
[Thinking block preservation by model](https://platform.claude.com/docs/en/build-with-claude/thinking#thinking-block-preservation-by-model):

> A thinking block is readable only by the model that produced it and
> certain other models. The API ignores or drops the blocks that the target
> model can’t read

> Don’t strip thinking blocks when you switch. The API drops unreadable ones
> without billing them, and a model that reads them still gets them.

And from [Troubleshooting](https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting#error-thinking-block-signature):

> A block from a model the target model can’t read never produces this
> error: the API drops it and, under the beta header, reports it in
> `input_transformations`.

These rules are about other Claude models. The docs say nothing about
blocks from other vendors. An OpenAI or Gemini signature is not an
Anthropic signature, so a converter must not send it as one.

Turning thinking off. From the
[per-model table](https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting#rejected-configurations):

> Models marked `Always on` cannot turn thinking off.

Always on: Claude Fable 5.x, Mythos 5.x, Opus 5.5 (`"disabled"` gets a 400).
Claude Sonnet 5.5 takes `between_tools` in place of `"disabled"`. Opus 5 and
Haiku 5.5 take `"disabled"` only at effort `high` or below. So "turn thinking
off for this request" is not a general fix. It is also a mid-turn toggle,
which the API itself replaces with "silently disables thinking".

No error entry exists for "must start with a thinking block". The API errors
page lists the modified-blocks, bound-signature, `enabled`/`adaptive`/
`disabled` not-supported errors only.

### 2. What Rig `0.46.0-rc.1` does

**Sameness.** `message/native.rs:55-101`: `Origin { api, provider, model,
response_model, response_id, context }`, and `same_model` compares `api`,
`provider` and `model`. `history.rs:1177-1185` (`Same::is`) also compares
`context` when the target binds thinking to its context. A turn without an
origin is never the same (`history.rs:1205-1208`).

**`adapt` contract.** `completion/history.rs:457-483`:

> - Any other turn keeps only canonical fields: provider items are cleared,
>   reasoning becomes plain text, or stays reasoning when the request asks
>   for it and the model reads it ([`Accepts::foreign_reasoning`]);
>   redacted or empty reasoning is dropped;
>   [`Opaque`] items are dropped and call ids go
>   through [`ReplayTarget::normalize_tool_call_id`], made distinct when two
>   calls would share one.

The code is at `history.rs:1243-1260`: `block.canonical()`, then
`Reasoning` → `None` if redacted or blank, else `Reasoning` if
`accepts.foreign_reasoning`, else `Text`. `Opaque` → `None`.

The test `completion/history/tests.rs:128-159`
(`another_model_replays_canonical_fields_only`) shows the result. A
foreign turn `[signed thinking, redacted, empty, opaque, text, call "c|1"]`
becomes `[Text("thinking"), Text("answer"), call "c_1"]`.

**Anthropic says it does not read foreign reasoning.**
`providers/anthropic/wire.rs:676-683`:

```rust
            // Thinking replays only with its signature.
            foreign_reasoning: false,
```

**Encoder.** `providers/anthropic/completion.rs:466-519` (`assistant_part`).
A block with a current provider item goes out verbatim, without
`estimated_tokens`. Otherwise it is rebuilt from canonical fields:

```rust
        // Thinking without its signature is text, as pi sends it. A
        // redacted block's payload lives only in its item.
        AssistantContent::Reasoning(reasoning) => {
            if reasoning.redacted || reasoning.text.trim().is_empty() {
                return None;
            }
            text(&reasoning.text)
        }
```

A `ToolCall` becomes `{"type":"tool_use","id","name","input"}`.

**The `thinking` field.** Rig does not change it for a foreign last turn.
It comes from the mapped options (`providers/anthropic/options.rs:22-90`):

- `Reasoning::Effort(e)` on an adaptive model →
  `{"thinking":{"type":"adaptive"},"output_config":{"effort":e}}` (lines 69-72).
- `Reasoning::Effort(e)` on a non-adaptive model → only
  `output_config.effort`, no `thinking` (lines 66-68).
- `Reasoning::Budget` → `{"type":"enabled","budget_tokens":N}` (lines 85-87).
- `Reasoning::Off` → `{"type":"disabled"}` (or the model's `thinking_off`
  value), or a local refusal on a model that cannot turn it off (lines 37-47).

On Anthropic's own API, for a model that binds thinking to context, the body
gets `block_binding: {prefix_mismatch_behavior: "drop_block"}` (and the
`thinking-binding-controls-2026-08-01` beta) in adaptive mode. If the
caller names no thinking, Rig adds `{"type":"adaptive", block_binding}`
(`completion.rs:144-171`, `202-204`; `wire.rs:559-562`).

**keelrs's mapping.** `keelrs/src/agent/rig/run.rs:384-394` maps every
`Thinking` level to an `Effort` or to `Off`. So keelrs never sends
`"enabled"`.

**Resulting request** (keelrs, Claude adaptive model, last turn from
OpenAI with a tool call):

```json
{
  "model": "claude-…",
  "thinking": {"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "drop_block"}},
  "output_config": {"effort": "…"},
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "…"}]},
    {"role": "assistant", "content": [
      {"type": "text", "text": "<OpenAI text, and its reasoning summary as text if any>"},
      {"type": "tool_use", "id": "call_abc", "name": "lookup", "input": {}}
    ]},
    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_abc", "content": [ … ]}]}
  ]
}
```

(`block_binding` is there only for models whose catalog entry has
`binds_context`.) The assistant turn has no `thinking` block. In adaptive
mode, the docs say none is needed. With a forced `"enabled"` budget, the
same body breaks the manual-mode rule. The docs say the API then turns
thinking off for the request.

Rig has a live-checked neighbour case. `providers/anthropic/completion/tests.rs:1755-1797`:
a turn whose signed thinking went to text, sent with
`between_tools`, "which Anthropic takes where a mismatched signature is
refused (checked live on Claude Sonnet 5.5)". No Rig test or cassette
covers a foreign *open* tool loop sent to Anthropic. The cassette
`rig-cassette/fixtures/cassettes/anthropic/adversarial/three_provider_round_trip.yaml`
has a closed loop only.

**Side effect for Claude→Claude switches.** Rig's sameness is an exact
model match. A switch from one Claude model to another also turns the
signed thinking into text. The docs ask for the opposite: "Don’t strip
thinking blocks when you switch". This is not a rejection. The new model
loses the old model's reasoning, and the text it gets is the old
reasoning written as if it were the reply.

### 3. Changes after the pin (v0.47.1, main)

`git diff v0.46.0-rc.1 v0.47.1` on `completion/history.rs`,
`completion/message.rs` and `providers/anthropic/` does not change this
behaviour:

- `adapt`'s foreign-turn rule is the same (`history.rs:463-491` at v0.47.1).
  New: an aborted turn is replayed as its text, "as a turn from no known
  model: its reasoning, its calls and the results answering them are
  dropped". At the pin it was dropped like a failed turn.
- `assistant_part` is the same (`completion.rs:477-530` at v0.47.1).
  `wire.rs:812` still has `foreign_reasoning: false`.
- New: `ReplayTarget::blob_slots` records where a provider item keeps its
  reasoning blob (Anthropic: `/signature`, `/data`). This adds metadata.
  It changes no replay rule.
- `main` is v0.47.1 plus one README commit.

### 4. What pi-ai and Flue do

**pi `main`** (`packages/ai/src/api/transform-messages.ts:119-169`).
Sameness is `provider`, `api` and `model` (lines 122-125). For another
model:

```ts
				if (block.type === "thinking") {
					// Thinking from another model is dropped. Signatures and encrypted reasoning are
					// only valid for the model that produced them, and sending readable thinking as
					// plain text makes the new model treat it as part of the visible reply.
					if (!isSameModel) return [];
```

It also deletes a tool call's Gemini `thoughtSignature` (lines 152-155) and
normalizes call ids (lines 157-163). Commit `911309429` (2026-10-15,
"drop thinking from other models instead of sending it as text", issue
#10995) made this change. The first release with it is `v1.3.0`. Test:
`packages/ai/test/transform-messages-cross-model-thinking.test.ts`. A foreign turn
`[thinking, text, toolCall]` becomes `[text, toolCall]`.

**pi `v1.1.0`** (the version Flue locks), `transform-messages.ts:101-117`.
It drops foreign redacted thinking and converts foreign readable thinking
to `text`. Rig at the pin does the same.

**pi's Anthropic encoder** (`packages/ai/src/api/anthropic-messages.ts:1693-1745`).
It sends `redacted_thinking` with `data`, sends `thinking` with
`signature`, and sends thinking that has no signature as `text`. It
does not change `thinking` for a foreign last turn
(`anthropic-messages.ts:1535-1569`). On models with per-message effort it
sends `adaptive` with `block_binding: drop_block` (lines 1539-1546).

**pi's handoff test** (`packages/ai/test/cross-provider-handoff.test.ts`).
It sends contexts from many providers to each target, but the loops are
closed (it ends with a final assistant reply), not open.

**Flue** keeps pi-ai's message and delegates conversion to pi-ai. The
assistant record keeps `modelInfo`: the pi `AssistantMessage` without
`role`, `content`, `stopReason`, `errorMessage`, `timestamp` and `usage`.
So `api`, `provider`, `model` and response metadata stay
(`packages/runtime/src/conversation-records.ts:145-162`). It also keeps
the per-block opaque data: `textSignature` (line 186), reasoning
`encrypted` and `redacted` (lines 204-211), and tool call
`thoughtSignature` (line 221). `packages/runtime/src/session.ts:345-368`
copies `thinkingSignature`, `redacted` and `thoughtSignature` into turn
context.

### 5. Gaps

- No source shows a live request with this exact shape: an open tool loop,
  foreign last turn, Anthropic, thinking on. The docs answer it for
  adaptive mode ("no assistant turn needs to start with one"). For manual
  mode the docs say it degrades, but the two sentences ("enforces" and
  "doesn't error") are not fully clear together. A live check would settle
  it, but keelrs never sends manual mode.
- Rig converts foreign reasoning to text. pi no longer does, for quality
  reasons. This does not cause a rejection, but keelrs may want the newer
  pi behaviour. Rig has a `foreign_reasoning` request flag, and Anthropic
  turns it off. No flag drops the reasoning instead.
