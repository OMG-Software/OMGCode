# 0076: Configure the tool-enabled turn timeout

- Status: Accepted
- Date: 2026-11-23
- Implemented: version 0.52.1
- Amends: [ADR-0074](0074-budget-model-requests-for-sequential-tool-calls.md)

## Decision

Default to 60 seconds for chat and accept whole-second deadlines from
1 to 3600. Use **Tool-enabled turn timeout (seconds)** in desktop **Chat settings**
or `chat --turn-timeout-seconds` in the CLI. The desktop can save the value as
a default. Older preferences and chat requests that omit it get 60 seconds.
When the runtime detects expiration, it reports the configured time limit.

Keep the runtime's cooperative accounting. It checks the deadline between
bounded operations, excludes desktop approval waits, and retains the separate
30-second timeout per model request. Pre-turn compaction and text-only chat
use their existing request budgets. The core library's default stays at 30
seconds, and the core caps any configured deadline at one hour.

## Alternatives

Scaling the whole-turn deadline with the call budget would make a larger call
budget silently allow a longer-running turn. Expose the time budget separately.
Forcing a hard stop in the middle of a broker operation would change its
cancellation and persistence contract. Check between operations instead.
