# 0074: Budget model requests for sequential tool calls

- Status: Accepted, amended by [ADR-0075](0075-configure-the-tool-call-budget-per-turn.md) and [ADR-0076](0076-configure-the-tool-enabled-turn-timeout.md)
- Date: 2026-11-23
- Implemented: version 0.52.1

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
limits available to runtime callers. Keep the caps of eight calls per response
and sixteen per turn, the byte limits, and the existing per-call checks.

Use the existing core hard maximum of five minutes as chat's cooperative
deadline to accommodate more requests. Each model request still has a
30-second timeout. The core library default stays at 30 seconds, and desktop
approval waits are excluded from the turn deadline as before.

Tell the model to read the file if `old_text` is unknown or an exact edit
returns `edit_no_match`. The broker still requires a unique literal match in
the live file.

## Alternatives

- Asking the model to batch its calls cannot help when a later decision depends
  on a read, a write result, or a recoverable failure.
- Retrying mismatched edits automatically would require guessing the intended
  replacement target. Require the model to use the ordinary read and edit
  tools for recovery.
