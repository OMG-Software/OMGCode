# 0087: Show a per-part context estimate for the open session

- Status: Accepted; amended by [ADR-0102](0102-choose-a-reasoning-effort-per-session.md)
- Date: 2026-11-24
- Implemented: the command, and the view as the right-hand panel's
  **Context** tab

## Context

The composer's meter shows one number after a turn: the conservative input
estimate, the compaction trigger, and the context capacity. It does not say what
the estimate is made of. An operator who sees the context fill cannot tell
whether the base prompt, custom instructions, tool definitions, a compaction
summary, or the conversation is the cause, and cannot see the cost of turning
tools on. Operator feedback asked for a "context breakdown" tab in the
right-hand panel of the `main` window.

The estimate exists in one place: `estimate_history_input_tokens` in
`omgcode-core`, applied by the chat preflight to the history the next request
would send. The desktop shell gives the webview no authority, so the webview
cannot assemble the parts itself.

## Decision

1. **One command.** `session_context_breakdown { session_id }` is a `main`-only
   command. Rust resolves everything from trusted state by session id: the
   journal through the index, the saved chat budgets and global instructions,
   the session workspace's saved instructions, and live settings for the tool
   decision (the same `select` a turn uses). It sends nothing to a model,
   records nothing in any journal, and returns names and numbers only, never
   instruction or message text. It runs on the blocking pool like every service
   command.
2. **One estimator.** Core splits `estimate_history_input_tokens` into
   `estimate_history_parts`, which returns the terms, and
   `InputEstimateParts::total`, the one sum. The old function returns that sum,
   so the preflight total does not change. The chat module's
   `estimate_next_turn_parts` applies the parts to `active_history`, the same
   history function the preflight uses, and splits them. A test shows the parts
   add up to the preflight total for the same session state, less the new
   user message the trigger also counts.
3. **Parts.** System prompt, global instructions, workspace instructions, tool
   definitions, compaction summary, conversation history after the compaction
   boundary (with its message count), selected skills, and message framing.
   The instructions label and the per-message overhead of the system and
   instruction messages belong to no part, so they are reported as `framing`
   and the parts still add up. When both instruction texts are saved, the
   headings and separator that join them count with the workspace part.
4. **Not included, with a reason.** A part that a turn would not send is listed
   with zero tokens, `included: false`, and a fixed reason code
   (`preference_disabled`, `workspace_unregistered`, `no_compaction`,
   `none_saved`, `selected_per_turn`, and the other codes of
   `tool_mode::select`). Skills are loaded only when selected for one turn, so
   they are never included.
5. **Totals.** The estimate, the output reserve, the effective trigger, the
   saved context capacity (`null` when none is saved, in which case the trigger
   is the saved threshold alone), and whether auto-compaction is on. They come
   from the saved chat budgets and composer preferences, which the composer
   sends with each turn.
6. **Unregistered workspace.** The command does not fail. As in `chat_send`
   since ADR-0081, a workspace that is no longer registered contributes no
   instructions, even when a saved file remains, so the workspace instructions
   part and the tool definitions part are not included, both with
   `workspace_unregistered`.
7. **Errors.** `not_found` for an unknown session; `busy` while a turn runs for
   the session here or another process holds the journal (the writer lock);
   `invalid_journal`; `instructions_unreadable`, which a turn also reports.
8. **View.** `createContextTab()` in `ui/src/main/context_tab.ts` returns a
   `PanelView` (`element`, `showSession`, `refresh`, `dispose`) for the
   right-hand panel framework to mount. It draws a stacked bar as an SVG with
   sizes in attributes (the page sets no inline style), marks the trigger,
   lists every part with tokens, share, and a note, shows the totals, and
   states "Conservative estimate before the next turn". Colours come from the
   shared tokens and every part is also labelled in text. All text goes through
   text nodes. A failed reload, such as `busy` just after a turn, keeps the
   session's last estimate below the error until a reload succeeds; a session
   change clears it first.

## Alternatives considered

- **Compute the parts in the webview from the transcript.** Rejected. The
  webview does not know the base prompt, the composed instructions, the tool
  allowance, or the replay rules, and a second estimator would drift from the
  one the trigger uses.
- **Extend `ContextMeterDto` and show the parts only after a turn.** Rejected.
  The meter describes one finished turn. The operator wants to see the next
  turn before sending, including after toggling tools or editing instructions.
- **Return the text of each part.** Rejected. It would send instruction and
  message text to the webview for no benefit, and the estimate needs only sizes.
- **Fold framing into the system part.** Rejected. The label and the
  instruction message's overhead are not the system prompt; naming them keeps
  every number attributable.
- **Fail the command for an unregistered workspace.** Rejected. A turn in
  such a session still runs as text-only chat, and the estimate is useful while
  the operator decides what to do about the registration.
- **Count the operator's draft message.** Rejected for now. The draft is not
  sent to Rust until the turn starts, and sending it for estimation would add
  a second path for untrusted text.

## Consequences

- The estimate is the conservative byte-based one: one byte counts as one
  token, so it overstates a real tokenizer's count. It is not provider-reported
  usage and is not updated while a turn runs. The panel calls `refresh()`
  when a turn settles while the tab is visible.
- The command opens the journal for reading its state, so a concurrent
  `chat_send` for the same session can see `busy` for the instant it is open.
- The command count rises by one.
- Verification: core and app unit tests for the sum, an integration test per
  case (plain, compacted, tools on and off, instructions, totals, unregistered
  workspace, unknown session), and jsdom tests of the view. Not checked in a
  running Tauri window.

## References

- [Desktop interface: Context tab](../desktop-interface.md#context-tab)
- [Context management](../context-management.md)
- [Agent instructions](../agent-instructions.md)
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)
