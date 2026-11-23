# 0077: Configure the tool-enabled turn timeout

- Status: Accepted, amended by [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)
- Date: 2026-11-23
- Implemented: version 0.52.1
- Amends: [ADR-0075](0075-budget-model-requests-for-sequential-tool-calls.md)

## Decision

Default to 120 seconds for chat and accept whole-second deadlines from
1 to 3600. Use **Tool-enabled turn timeout (seconds)** in desktop **Chat settings**
or `chat --turn-timeout-seconds` in the CLI. The desktop can save the value as
a default. Older preferences and chat requests that omit it get 120 seconds.
Chat used 120 seconds before the timeout was configurable, and each model
request may take up to 30 seconds.
When the runtime detects expiration, it reports the configured time limit.

Keep the runtime's cooperative accounting. It checks the deadline between
bounded operations, excludes desktop approval waits, and retains the separate
30-second timeout per model request. Pre-turn compaction and text-only chat
use their existing request budgets. The core library's default stays at 30
seconds, and the core caps any configured deadline at one hour.
The call budget and timeout are independent. A slow model can exhaust the
120-second default before using every allowed tool call.

## Alternatives

A 60-second default was proposed before release and rejected. At about ten
seconds per local model request it allows roughly six requests. The turn that
prompted ADR-0075 (list, create, a failed edit, read, two more writes, and the
final reply) needs seven, so it could still stop before finishing.

Scaling the whole-turn deadline with the call budget would make a larger call
budget silently allow a longer-running turn. Expose the time budget separately.
Forcing a hard stop in the middle of a broker operation would change its
cancellation and persistence contract. Check between operations instead.
