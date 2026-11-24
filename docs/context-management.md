# Context management and caching

Version 0.26.0 implements a bounded subset: conservative accounting, a visible
meter, manual `session compact`, and pre-turn auto-compaction for the local LM
Studio route. The remaining sections are the release contract; caches,
mid-turn compaction, provider-native compaction, persisted settings, and
memory-driven context remain planned. Apply the
[instruction contract](agent-instructions.md), [skills contract](skills.md),
and [security rules](security-coding-rules.md).

## Implemented (0.26.0)

```text
omgcode chat ... --compact-threshold 200000 --auto-compact "request"
omgcode session compact <journal> --endpoint URL --model MODEL \
    --context-tokens N --summary-tokens 2048
```

- Chat reports `context.estimated_input_tokens`, `context_tokens`,
  `max_output_tokens`, `effective_trigger`, `auto_compaction`, and the active
  compaction boundary in `context.compaction`.
- The conservative estimate counts input only: the compiled base prompt,
  custom instructions, active history, message overhead, and advertised tool
  definitions. It treats one UTF-8 byte as one token, so it is deliberately
  not an exact tokenizer count. The output reserve and template headroom are
  subtracted once, from the context, to give the safe input budget.
- The trigger is the smaller of `--compact-threshold` and that safe input
  budget. Compaction is also due when active history plus the pending request
  would exceed the prompt-assembly limits (640 history messages or 256 KiB
  assembled). Tool-enabled chat also reserves two history messages per
  configured tool call. Without `--auto-compact`, chat refuses an over-budget
  turn before the provider call and suggests manual compaction. Lowering the
  tool-call limit also reduces the history reserve.
- `session compact` requires an idle, unarchived session. It summarizes active
  messages before the latest user message, retains the latest user message and
  all later assistant/tool messages, and records a schema-v3
  `context.compacted` event. The original journal messages remain.
- Saved tool results reach the summarizer only under live `model_tools` and
  `read`; otherwise they are withheld placeholders. The record states whether
  tool results were included, and such a summary is withheld from any later
  turn, including text-only chat, that lacks the same live authority.
- The summary is labelled user-level untrusted evidence, not an instruction.
  It is bounded to 8 KiB, and the summarizer input and output are redacted for
  the configured local token.
- Summarizer input is split at message boundaries into bounded chunks; multiple
  summaries are combined with hard chunk and depth limits. A chunk is sized from
  the summary request's own output reserve, after deducting the compiled base
  prompt, the compaction instructions, and their message overhead. A chunk,
  including one in a combination round, holds at most 128 history messages.
  Many short messages are split into several bounded chunks. Empty,
  oversized, or failed summaries fail closed, and a summary that would not
  shrink the active context is refused with nothing recorded.
- When the summarizer input carries tool-derived content, live `model_tools`
  and `read` are rechecked before each summarizer request. A withdrawal stops
  compaction with a `permission` error before the next request, and nothing is
  recorded.

Selected skills (`chat --skill`, ADR-0057) are request-only user-level
messages after the saved preferences. The context estimate counts them, but
they are not history: compaction never summarizes them and later turns never
replay them.

Not implemented: mid-turn compaction after tool results, persisted workspace
settings, provider-native opaque compaction, caches, and memories. The desktop
shell shows the context meter line after each turn (estimated tokens, context
size, and the compaction trigger). Since 0.40.0 it saves the chat
preferences, including the compaction threshold and the automatic
compaction switch, which grant nothing. It has no live meter while a turn
runs, and it shows the meter only for a turn that reports one. A
`session_context_breakdown` command and a Context tab view estimate the open
session's next turn by part (system prompt, global and workspace instructions,
tool definitions, compaction summary, retained history, and fixed framing) from
the same arithmetic as the pre-turn trigger, without a model request or a
journal write. The view is built but not yet mounted in the window
([ADR-0087](adr/0087-show-a-per-part-context-estimate-for-the-open-session.md)).

## Settings and accounting

Provide auto-compaction on/off, a positive integer threshold in tokens, a manual
Compact now action, and a visible context meter. `200k` means **200,000 tokens**.
Treat it as a requested trigger, not a model context-window size. Show the
configured threshold and any lower effective trigger with its reason.

Count the complete assembled input: base prompt, user additions, selected skill
content, history, summaries, retrieved material, tool definitions, tool results,
and provider-specific overhead. Cached input still occupies context. Include
non-text input using the adapter's supported accounting. Use provider token
counting or a compatible tokenizer where available; label conservative estimates
and their uncertainty instead of presenting them as exact counts.

Before every request, resolve the selected model, endpoint, authentication mode,
and input/output limits. Reserve the requested output, any separately required
reasoning allowance, and an accounting safety margin according to that route's
rules. Avoid counting the same allowance twice. Let B be the remaining safe
input budget and T the configured threshold. Trigger compaction when assembled
input is **at or above min(T, B)**. Recompute after a provider/model change.

For example, T=200,000 and B=240,000 triggers at 200,000. If B=110,000, the
effective trigger is 110,000 and the UI must explain the earlier compaction.
If required limit metadata is unavailable, require a validated conservative
profile before continuing. Never assume every connection supports 200k.

Check growth after new messages and tool results and before the next model call.
Bound tool output before inserting it. Preserve a reference to omitted output
and disclose truncation. When auto-compaction is off, stop before an oversized
request and offer manual compaction or a compatible model.

## Portable compaction

Use harness-managed, readable summaries as the initial cross-provider path.
Compact older history while retaining recent exchanges and valid tool-call/result
pairs. Do not rewrite fixed instructions or drop pending user messages to fit.
The original journal remains available under an explicit retention policy;
compaction changes active context, not the recorded history.
The [session storage contract](session-storage.md) defines that JSONL journal,
archive state, and recovery. Compaction does not archive or delete a session.
Selected memories retain their IDs, revisions, scope, and original source trust.
Use the current [memory registry](memories.md) to resolve enabled entries after
compaction; a summary cannot save, enable, or promote a memory.

A compacted record must preserve:

- The current goal, user corrections, constraints, and decisions with sources.
- Relevant file/content versions, completed checks and their actual outcomes.
- Pending work, unresolved questions, failures, and uncertain external outcomes.
- Original message/tool references, source trust, and the instruction manifest.

Store approvals, permission policy, tool lifecycle state, and skill enablement in
core-owned records outside generated summaries. Revalidate them independently.
A summary cannot approve a command or mark a task complete. Preserve hostile
quotations as labelled evidence; do not promote them into trusted instructions.
Treat a user-edited summary as user-supplied context, not as an edited audit log.

Publish the new active context transactionally against its journal revision.
Include messages that arrive during compaction before the next request. If the
source changed, reconcile the summary rather than losing user steering. Show the
compaction event, before/after counts, summary, and retained source references.

Use the current authorized model profile for summarization. A different model
or account requires an explicitly selected permitted profile. Count compaction
calls, tokens, time, and charges in session usage. Summarize in bounded chunks
that fit the summarizer's own input budget. Use an initial target below 60% of
the effective trigger to avoid compaction on every turn; this is a design target,
not a claim that every history can be reduced to that size.

Bound retries. If compaction fails, makes no useful reduction, or fixed
instructions plus required recent state cannot fit, pause with an actionable
reason. Never loop indefinitely, silently remove mandatory instructions, or
send a request known to exceed the budget. Reconcile incomplete tool operations
from core records; never replay them because a summary mentions them.

Provider-native compaction is an optional adapter capability. Validate it for
the exact model and authentication route. Opaque provider state is not a portable
summary and cannot be presented as readable text. Keep its canonical protocol
items intact, label the visibility limit, and retain the local journal. Do not
forward provider-specific state to another provider; rebuild authorized context.

## Cache classes

| Class | Planned use and limits |
| --- | --- |
| Provider prompt cache | Reuse stable prompt prefixes when supported. Keep base instructions and tool ordering stable. Track reported cache reads/writes; hits and billing effects remain provider-specific. |
| Local immutable artifacts | Reuse parsing, indexes, and summaries keyed by exact content and algorithm/prompt versions. Check current access and source validity before use. |
| Local metadata | Cache capability discovery with an explicit expiry and revalidation. An expired or uncertain limit must not authorize an oversized request. |
| General model responses | Off by default. Do not memoize ordinary agent turns or execute cached tool calls. A later pure-analysis cache needs a separate reviewed contract. |
| Action ledger | Record operation identity and outcome for recovery/deduplication. This is not a cache that makes an old action a fresh result. |

Local cache lookup must include the relevant user, workspace identity, active
root, policy revision, content digest, artifact version, instruction/skill
manifest, and provider/account/model/settings where applicable. Conversation
artifacts remain session-scoped. Reuse a pure content artifact across sessions
only within the same permitted workspace and user. Do not share task context
between workspaces, accounts, or users through a matching content hash alone.

User-saved global memories have explicit cross-project eligibility under the
[memory contract](memories.md). That eligibility does not share project artifacts
or conversation caches. Include selected memory IDs/revisions/scopes in context
cache keys and invalidate affected entries after memory edit, withdrawal, or
scope/project changes.

A hash establishes content identity, not authorization or source trust. Validate
the current file object/version, permissions, and provenance before returning an
entry. Missing sources, permission revocation, changed roots, edited instructions,
and changed or disabled skills invalidate affected lookups and derived context.
TTL alone is insufficient. Keep caches outside task-writable storage, apply
per-user access controls, and validate corrupted or replaced entries.

Exclude credentials and authentication responses from the general cache. Treat
cached source code and summaries as sensitive data with explicit retention.
Sign-out disables use of entries tied to that provider account. Do not cache
build/test/command/Git outcomes as proof of a fresh execution, skip authorization
on a cache hit, or replay writes, pushes, logins, or other side effects.

Provide per-workspace quota, expiry, usage, and clear controls, plus a global
clear action. Remove affected local artifacts and pending reuse when cleared.
Local clearing does not prove that a provider's remote cache or retained data
has been deleted. Explain provider retention separately. Build/package caches
have a separate [toolchain contract](development-tools.md).

## Efficiency and acceptance

Report actual requests, input/output tokens, provider-reported cached tokens,
compaction overhead, local cache hits, and latency. Label unavailable costs;
do not infer a price or quota saving from a local hit. Compare representative
tasks with caching/compaction enabled and disabled while checking correctness.

Required cases include just below/at/above 200,000 tokens; smaller model budgets;
estimated counts; large tool results; disabled compaction; model/account changes;
summary injection; new messages during compaction; failure/no-progress limits;
preserved tool pairs and unknown outcomes; cache poisoning; changed file/link
targets; permission revocation; instruction/skill edits; sign-out; isolation;
quota eviction and clear; and absence of cached side-effect execution.

## Provider evidence

These sources establish capability differences, not universal adapter support:

- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
  describes prefix reuse. A cache hit does not reuse a previous output response.
- [OpenAI compaction](https://developers.openai.com/api/docs/guides/compaction)
  describes native compaction with opaque items and canonical output handling.
- [Claude prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
  documents cache controls for the Messages API.
- [OpenRouter model metadata](https://openrouter.ai/docs/api/api-reference/models/get-models)
  exposes model-specific context limits.

API documentation does not establish subscription-route feature availability.
Recheck current contracts and test each adapter before enabling a capability.
