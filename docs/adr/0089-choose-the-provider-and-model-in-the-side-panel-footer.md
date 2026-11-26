# 0089: Choose the provider and model in the side panel footer

- Status: Accepted; amended by [ADR-0102](0102-choose-a-reasoning-effort-per-session.md)
- Date: 2026-11-24
- Amends: [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md) decisions 6 to 11, for the placement only

## Context

ADR-0071 put the per-session **Provider** dropdown, the **Model** dropdown, and
their notes in the composer toolbar. Operator feedback called that placement
ugly and asked for the choice in a box at the bottom of the right-hand panel,
as a permanent fixture. ADR-0083 had already given that panel a fixed footer
under every tab, empty until a feature used it.

## Decision

1. A model box, built by `ui/src/main/model_box.ts`, fills the side panel's
   fixed footer. It stacks a "Model" heading, the **Provider** dropdown, the
   **Model** dropdown with its **Refresh** button, and the notes (list status,
   off-machine warning, connection message, unreadable saved choices). It is
   outside the scrolling tab content and stays when the tab changes.
2. Behaviour is unchanged. The box uses the same shared connection state
   (`connection_state.ts`), the same commands (`connection_select` with its
   native Yes/No confirmation, `session_model_select`, `chat_models`), the same
   per-session semantics and persistence, the same messages, and the same
   disabled states: both dropdowns are disabled while no session accepts a
   message and while a turn runs. No command, IPC shape, permission, or Rust
   code changed; only the `main` capability's description and a comment in
   `tests/layers.rs` now name the model box.
3. The composer no longer holds these controls. It receives the box as an
   explicit dependency and reads the chosen model from it for each send; it
   still reads the endpoint from the shared connection. The composer tells the
   box whether it is enabled, and refreshes when the box's model or list
   changes.
4. A collapsed panel hides its footer, so the rail gets an icon button named
   "Model" (tooltip and accessible name) that expands the panel and shows the
   box. The existing rail buttons stay. `createSidePanel` takes optional rail
   actions for this; the panel itself knows nothing about models.
5. ADR-0071's composer placement is superseded. Its per-session semantics,
   the stale handling, and the off-machine warnings stay in force.

## Alternatives considered

- **Keep the controls in the composer and restyle them.** Rejected: the
  feedback asked for them in the panel, and the toolbar is narrow in the
  conversation column.
- **A new tab in the panel.** Rejected: the choice must be reachable from
  every tab without switching, and a tab hides when another is selected.
- **Let the composer read the dropdowns from the DOM.** Rejected: the send
  would depend on markup elsewhere in the page. An explicit dependency keeps
  one source of truth.
- **Move the state into the connection model.** Rejected as a larger change
  than the placement needs; the picker already holds the chosen model and the
  connection model already holds the recorded one.

## Consequences

- The conversation column has more room for the toolbar.
- A narrow panel stacks the controls vertically and long model ids are
  truncated by the dropdown, not wrapped.
- Collapsing the panel hides the choice until the rail's **Model** button
  expands it; a send still uses the last choice shown.
- The box is created once in `app.ts`, outside the conversation, so it
  survives the conversation's own renders.

## References

- [Desktop interface](../desktop-interface.md#right-hand-panel)
- [ADR-0066](0066-choose-the-chat-model-from-the-local-servers-model-list.md),
  [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md),
  [ADR-0083](0083-show-a-names-only-workspace-file-tree-in-the-right-panel.md),
  [ADR-0084](0084-drop-the-status-bar-connection-choice.md)
