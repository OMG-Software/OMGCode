# ADR-0074: Give chat bubbles a shared width

- Status: Accepted
- Date: 2026-11-23

## Context

[ADR-0072](0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md)
centred transcript entries at 90% width and let operator messages shrink to
their text. This gave messages different left edges within the same chat.

## Decision

Centre transcript entries at 96% of the available width. Use that width and
left-align text for operator messages and live and recorded replies. The
remaining width provides equal gutters on both sides of the column, so the
transcript needs only vertical padding.

## Consequences

Message edges line up as the conversation panel resizes. Divider notes and
explicit Markdown table alignment keep their own text alignment.
