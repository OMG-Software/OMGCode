# 0075: Budget model requests for sequential tool calls

- Status: Accepted, amended by [ADR-0076](0076-configure-the-tool-call-budget-per-turn.md) and [ADR-0077](0077-configure-the-tool-enabled-turn-timeout.md)
- Date: 2026-11-23
- Implemented: version 0.52.1 as amended by ADR-0076 and ADR-0077

## Context

A desktop turn listed the workspace, created `hello.cpp`, and attempted an
exact edit with `[Existing content]` as `old_text`. The edit returned the
recoverable `edit_no_match` result. The fourth model request asked for another
tool call, but the round limit refused it to reserve a final reply. The turn
stopped before it could write the tests. Three sequential tool calls could
use the entire tool budget in practice even though the turn allowed sixteen
calls when batched.

## Decision

Set the maximum model requests to the maximum tool calls plus one. A turn can
use one request per call and still request a final reply. Keep explicit lower
limits available to runtime callers. Keep the cap of eight calls per response,
the byte limits, and the existing per-call checks. ADR-0076 replaced the
original 16-call cap before release.

The original five-minute chat deadline was replaced before release by the
configurable deadline in ADR-0077. Each model request still has a 30-second
timeout. The core library default stays at 30 seconds, and desktop approval
waits are excluded from the turn deadline as before.

Both edit tools tell the model to read the file if `old_text` is unknown or the
tool returns `edit_no_match`. The broker still requires a unique literal match
in the live file for each replacement.

## Alternatives

- Asking the model to batch its calls cannot help when a later decision depends
  on a read, a write result, or a recoverable failure.
- Retrying mismatched edits automatically would require guessing the intended
  replacement target. Require the model to use the ordinary read and edit
  tools for recovery.
