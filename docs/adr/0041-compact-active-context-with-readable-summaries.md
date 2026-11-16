# 0041: Compact active context with readable summaries

- Status: Accepted
- Date: 2026-11-15

## Context

The local LM Studio chat route had a caller-supplied context budget and a
conservative byte-based estimate, but no way to reduce a long session. The
[context contract](../context-management.md) requires readable, harness-managed
compaction, original-history retention, tool-pair preservation, bounded
summarizer calls, and a visible trigger and meter. It also requires that a
summary never grants authority or replaces core-owned state.

## Decision

Add schema-version-3 session compaction and a pre-turn context trigger:

1. **Journal:** a `context.compacted` event records the readable summary, the
   number of messages it replaces, the summary digest, the summarizer model,
   and optional provider usage. It requires an idle, unarchived schema-v3
   session and a boundary later than any prior compaction. The original
   messages stay in the journal.
2. **Active context:** the synthetic summary is a labelled user-level message;
   it is untrusted evidence, not instructions. Active history is the summary
   followed by the messages after the recorded boundary.
3. **Cutoff:** compaction retains the latest user message and every later
   assistant/tool message, preserving the current request and its tool pairs.
   Older messages are summarized.
4. **Trigger:** `chat` accepts `--compact-threshold` (default 200,000) and
   `--auto-compact`. The effective trigger is the smaller of the threshold and
   the safe input budget after the output reserve and template headroom. The
   input-only estimate counts the compiled base prompt, custom instructions,
   active history, message overhead, and advertised tool definitions, so no
   allowance is counted twice. It treats one UTF-8 byte as one token and is
   labelled conservative. Compaction is also due when the pending request
   would exceed the prompt-assembly message or byte limits.
5. **Manual action:** `session compact` summarizes the active journal with the
   selected local model. `chat` without `--auto-compact` refuses an oversized
   pre-turn estimate and points to manual compaction.
6. **Summarizer:** the current authorized local model is used with a bounded
   output allowance. Input is split at message boundaries into bounded chunks
   sized from that allowance and the complete fixed summarizer request;
   multiple chunk summaries are combined recursively with hard chunk and depth
   limits. A failure, empty result, oversize summary, or a summary that would
   not shrink the active context is a controlled failure that records
   nothing. The configured local server token is redacted from the
   summarizer input and output before recording.
7. **Tool-result authority:** a saved tool result is workspace data disclosed
   under an earlier decision ([ADR-0026](0026-offer-read-list-tools-through-a-workspace-preference.md)).
   The summarizer receives saved tool results only under live `model_tools`
   and `read`; otherwise they are withheld placeholders. The record's
   `includes_tool_results` states whether the summary was built from them.
   Such a summary is replayed under the same live rule and is otherwise
   replaced by a withheld notice. While tool-derived input is pending, the
   live rule is rechecked before every summarizer request; a withdrawal stops
   compaction before the next request. Chat and the turn runtime use one replay
   function for the active context, so neither can bypass the rule.
8. **Validation:** the stored digest must match the summary, and the first
   retained message must be a user message, so active history never starts
   inside a tool exchange.
9. **Output:** chat reports the conservative input estimate, configured context
   and output budgets, effective trigger, auto-compaction state, and the active
   compaction boundary and digest.

## Alternatives considered

- **Delete or rewrite older journal messages.** Rejected: it destroys the
  recorded history and recovery evidence.
- **Inject the summary as a system instruction.** Rejected: model output is
  untrusted and cannot become instructions.
- **Always keep the full history and fail at the provider limit.** Rejected:
  it gives no manual or automatic recovery path.
- **Use a different model for summary without operator selection.** Rejected:
  the current authorized route is used, and a different model would need an
  explicit profile choice.
- **Always summarize tool results.** Rejected: the summary would carry
  workspace data past a later revocation of `read`, bypassing ADR-0026.
- **Never summarize tool results.** Rejected: the summary would lose file
  versions and check outcomes that the contract asks it to preserve.
- **Unbounded recursive summary.** Rejected: chunk count, depth, summary size,
  and calls are bounded.
- **Provider-native opaque compaction.** Deferred: it is an optional adapter
  capability and cannot be presented as a readable cross-provider summary.

## Consequences

- New sessions use schema version 3. Version-1 and version-2 journals remain
  readable; a version-2 journal cannot contain a compaction event.
- Compaction changes active context only. Archiving, recovery, and the original
  messages are unchanged.
- A summary cannot approve a command, complete a task, enable a skill, or
  revive a tool result. Core-owned policy and tool state are revalidated.
- The pre-turn trigger does not yet compact between tool rounds. The runtime's
  existing prompt bound still refuses a request that grows too large after a
  tool result.
- After `read` is revoked, a tool-derived summary is withheld whole, so the
  model loses all compacted context until authority returns. Compaction run
  without authority loses the tool content from the active context for good;
  the original journal still holds it.
- Auto-compaction settings are CLI flags, not persisted workspace settings.
- Caches, provider-native compaction, memories, and mid-turn compaction remain
  future work under the context contract.

## References

- [Context management](../context-management.md)
- [Session storage](../session-storage.md)
- [Provider authentication](../provider-authentication.md)
- [Security coding rules](../security-coding-rules.md)
- [ADR-0005](0005-keep-agent-context-subordinate-to-runtime-policy.md)
