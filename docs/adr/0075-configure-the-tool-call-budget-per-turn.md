# 0075: Configure the tool-call budget per turn

- Status: Accepted
- Date: 2026-11-23
- Implemented: version 0.52.1
- Amends: [ADR-0074](0074-budget-model-requests-for-sequential-tool-calls.md)

## Decision

Expose one per-turn budget for all model tool calls, including calls in batches
and recoverable failures. Default to 128 and accept whole numbers from 1 to
256. Budget one model request per permitted call and one for the final reply.
The desktop composer takes this value from **Chat settings** and can save it
as a default. A saved file that predates the setting reads as 128. The CLI
takes the value from `chat --max-tool-calls` for the current turn.

If a proposed batch would cross the cap, the core refuses the whole batch
before dispatch. The `tool_limit` message reports the configured cap, calls
used, and calls requested. Previous actions keep their recorded outcomes.
The cap grants no new permissions. The tool authorization, byte limits, and
deadlines still apply.

Allow 640 history messages so a request can carry 128 messages through the
user request and up to two messages per call at the 256-call maximum. Before
the first request, require room for the pending user message and two messages
per configured tool call. This can trigger compaction before any tool runs.
The 256 KiB assembled text limit and provider context limit still constrain
every request. Summarizer chunks remain capped at 128 messages.

## Alternatives

- A sequential-only counter could be bypassed by sending calls in batches.
  Count all calls against one budget and retain the separate per-response cap.
- An unbounded setting would let a turn consume unbounded model requests and
  perform unbounded authorized operations. Keep a hard maximum.
