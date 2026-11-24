# 0084: Drop the status bar connection choice

- Status: Accepted
- Date: 2026-11-23

## Context

ADR-0064 put a **Model** provider dropdown and a connection message in the
`main` window's status bar. ADR-0071 then added the same provider choice, over
the same shared state, to the composer, and per session. The status bar's copy
is redundant: operator feedback asked for it to go.

## Decision

1. The status bar no longer has a provider dropdown or a connection message. It
   shows the open session's workspace, its Git branch, and the approvals
   segment. This supersedes the status bar choice in ADR-0064 and decision 11
   of ADR-0071 for the footer; the rest of both records stands.
2. The connection message is not lost. The model box in the side panel
   footer (ADR-0089) shows every connection failure and the "The model
   provider was not changed." note beside its own dropdowns, from the same
   shared connection state. A collapsed panel hides the box, and its rail's
   **Model** button shows it again; the composer still names a problem that
   blocks a send, such as a stale provider or no chosen model, when the
   operator tries to send. The status bar therefore keeps no text for it.
3. `connection_select` stays in the `main` capability
   (`allow-connection-select`) because the model box's **Provider** dropdown
   still calls it. The command, its native Yes/No dialog, and its checks are
   unchanged. Only the capability description and the comment in
   `tests/layers.rs` now name the model box instead of the status bar.
   `connection_status` and `session_model_select` are unchanged.
4. The footer still asks the shared connection state to read again when the
   window regains focus and after a turn settles, so the model box follows a
   connection changed in the `settings` window.

## Alternatives considered

- Keep a read-only text label of the provider in the status bar: rejected, as
  the model box shows the same information and the feedback called the status
  bar entry redundant.
- Remove `allow-connection-select` from `main`: rejected, because the
  model box would then lose its provider choice. Least privilege is already
  met while that call exists.
- Delete the shared connection model or `createProviderSelect`: rejected;
  the model box uses both, and the composer reads the shared state.

## Consequences

- A smaller status bar and one control per choice. No permission changes.
- The provider choice is reachable only from the model box (ADR-0089), in
  the `settings` window, or from the CLI.
- A later move of the provider dropdown must keep `connection_select` behind
  the native dialog and keep `allow-connection-select` in `main`, as ADR-0089
  did.

## References

- [ADR-0064](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)
- [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md)
- [ADR-0089](0089-choose-the-provider-and-model-in-the-side-panel-footer.md)
- [Desktop interface](../desktop-interface.md)
