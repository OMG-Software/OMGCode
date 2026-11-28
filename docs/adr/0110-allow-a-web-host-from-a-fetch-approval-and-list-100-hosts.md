# 0110: Allow a web host from a fetch approval and list 100 hosts

- Status: Accepted
- Date: 2026-11-28
- Amends: [ADR-0098](0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)

## Context

[ADR-0098](0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md) gives
each workspace an allowlist of at most 32 host patterns. While `web_fetch` is
`allow`, a fetch to a listed host runs without an approval, and a fetch to any
other public host needs one. To stop being asked about a host, the operator
approved the fetch in the approvals window, opened the `settings` window,
found the workspace, and typed the host again. The operator asked for a third
button, **Allow host**, on a web fetch approval that adds the host to the
list, and for room for 100 hosts, because 32 is too small.

Two existing rules shape the design:

- An approval record is bound to the settings revision under which it was
  requested, and consuming it fails once the revision has changed
  ([agent instructions](../agent-instructions.md#approval-records)). Adding a
  host appends a settings event and advances the revision. A pending record
  is therefore stale after the host is added, and a record approved before
  the add can no longer be consumed.
- Under `prompt` every fetch needs an approval, whatever the host. Listing a
  host changes nothing there.

## Decision

### Allow host

- The approvals window shows **Allow host** between **Deny** and **Approve**
  only for an exact, pending `network` review whose workspace's `web_fetch`
  state is `allow`, whose host is not already on the list, and whose list has
  room. Core's review decides this. The approval detail reports
  `allow_host` as `offered` with the host, `not_allow`, `full`, or
  `unavailable`. For `not_allow` and `full` the window shows a note and no
  button. The button follows the same one-second delay as **Approve**.
- The host is the exact host of the reviewed URL, as a canonical exact
  pattern. It is never a wildcard: allowing `docs.example.com` lists that
  host only.
- Core builds the host, a `ReviewedHost`, while reviewing the approval, from
  the request that reproduced the record's digest. The desktop service keeps
  it with the single-use review token. `approval_decide` takes a third
  decision, `allow_host`, and redeeming the token returns the host. The
  webview sends only the decision and the token, and a request that carries
  a host field is refused. No Tauri command or window permission is added.
- Rust shows a native confirmation, modal to the approvals window. It names
  the host and the workspace folder and says three things: later fetches to
  that host run without asking while `web_fetch` is `allow`, whatever their
  path or query; its subdomains are not included; and other pending
  approvals go out of date. The token is spent before the dialog opens. A
  declined dialog changes nothing, and the window reviews again with a new
  token.
- After the dialog, Rust checks the record and the offer again. Core then
  appends the approval and, after it, the host, in one hold of the settings
  store. The approval is appended at the current revision, and the host
  advances it. The waiting turn sees the approval and dispatches the call
  again. Because the host is now listed, the fetch runs under the standing
  allow, and the model action journal records it as such. The approved
  record is never consumed.
- As for every settings change from this application, one change runs at a
  time. A settings change started in another window while the dialog is open
  is refused with `settings_change_running`.

### 100 hosts

`MAX_WEB_FETCH_HOSTS` rises from 32 to 100 patterns per workspace. The trusted
CLI, the `settings` window, and **Allow host** share the limit.

## Alternatives considered

- **Offering Allow host under `prompt`.** Rejected. A listed host still asks
  under `prompt`, so the button would change a setting with no visible
  effect, and the call dispatched again would ask again.
- **Listing a wildcard for the host's parent domain.** Rejected. It would
  allow hosts the operator never read. The `settings` window still accepts a
  typed wildcard.
- **Letting the webview send the host.** Rejected. The webview has no
  authority ([ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)).
  The host must come from core's review of the record's digest.
- **A separate Tauri command.** Rejected. It would need a new window
  permission for a decision that `approval_decide` already carries with a
  single-use token.
- **Adding the host first and approving second.** Rejected. The add advances
  the revision, so the pending record would be stale before it was approved.
- **Removing the limit.** Rejected. The settings log and every fetch's
  allowlist check would grow without bound. A limit of 100 meets the request
  and keeps the list readable.

## Consequences

- An operator can approve one fetch and stop being asked about its host in
  one step, after one native confirmation.
- While `allow` holds, a listed host receives any path or query the model
  sends, without review. **Allow host** therefore widens what a
  prompt-injected model can send to that host. The confirmation says so.
- Every other pending approval becomes stale, because the settings revision
  changes.
- The approved record stays unconsumed until it expires.
- If the host cannot be appended after the approval was, the approval stands
  and the error is returned. The revision is then unchanged, so the waiting
  turn consumes the approval and runs the fetch once, as after **Approve**.
- The CLI has no combined decision. `approvals approve` followed by
  `workspace web-host-add` has the same effect.
- Older OMGCode versions refuse a settings log in which a workspace lists
  more than 32 hosts.
- Automated tests cover core's review and decision, the desktop service with
  a scripted dialog, the wire contract, and the window against a mocked
  backend. The native dialog in the approvals window and a waiting desktop
  turn against a real site were not exercised.

## References

- [Workspace permissions](../workspace-permissions.md#web-fetch-adr-0098)
- [Desktop interface](../desktop-interface.md#approvals)
- [ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)
- [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
- [ADR-0098](0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)
