# 0048: Bind a stored credential to its profile's endpoint

- Status: Accepted
- Date: 2026-11-16

## Context

ADR-0047 lets the local LM Studio adapter read its token from the credential
bound to an operator-selected provider profile. It left one limit open: a
profile records a provider kind and an authentication mode, but no endpoint.
The endpoint still came from the command line, so a stored token was sent to
whichever literal loopback endpoint the operator named for that run.

Loopback-only validation limits the damage today. It will not once any
non-loopback provider exists. The
[authentication contract](../provider-authentication.md) says to scope a
credential to the correct provider and audience, and never to redirect
requests or credentials silently. This decision closes the limit before a
cloud adapter is written.

These constraints shape it:

- **Profiles are journal records.** A profile is stored inside the
  `provider_profile_added` control event. The record rejects unknown fields,
  and existing journals must keep replaying with their exact bytes.
- **A credential must not be re-pointed.** If an endpoint could be edited
  while a credential is bound, one settings change would send an existing
  secret to a new destination.
- **Only one adapter exists.** No rule for a cloud endpoint can be tested yet.
- **The desktop fixes its endpoint at launch** (ADR-0045), and the webview
  compares each chat request with it.

## Decision

1. **A profile can record an endpoint.** `provider-profile-add` accepts
   `--endpoint <url>`. The profile record gains an optional `endpoint` field.
   It is written only when present, so a journal without one keeps its exact
   bytes and still replays. `provider-profile-show` and `status` report it. An
   endpoint is not a secret.
2. **The endpoint is fixed for the profile's life.** There is no command that
   changes it. To use another endpoint, remove the profile and add a new one.
   Removing a profile already records the unbinding and deletes its
   credential, so a stored credential can never be moved to another endpoint.
3. **Validation at record time, by provider kind.** An `lm_studio` endpoint
   must pass the same rule the local adapter enforces on every request: plain
   `http`, a literal loopback address, an explicit non-zero port, no user
   information, query, or fragment, and the path `/v1`. Core has one
   implementation of that rule, used by both. Replay applies it too, so a
   journal cannot introduce an endpoint the command would refuse. Every other
   provider kind refuses an endpoint for now, because no adapter exists to
   define its rule.
4. **A selected profile supplies the endpoint.** With `--provider-profile`,
   the adapter uses the profile's recorded endpoint and nothing else. A
   selected profile with no recorded endpoint is refused with
   `endpoint_not_recorded`, whatever its authentication mode. Core returns the
   endpoint from the same call that decides whether the credential may be
   used, so the two cannot be taken from different places.
5. **`--endpoint` and `--provider-profile` are mutually exclusive.** Giving
   both is a usage error in `omgcode chat`, `omgcode session compact`, and
   `omgcode-desktop`. There is no "allowed when equal" case.
6. **The desktop endpoint stays fixed for the process.** With a profile, the
   shell resolves the endpoint at launch and shows the webview that value. Each
   turn resolves the profile again. If the recorded endpoint now differs from
   the launch value, because the profile was removed and added again, the turn
   is refused with a `credential` error that says to restart.
7. **Without a profile, nothing changes.** `--endpoint` and its default, and
   the `OMGCODE_LM_TOKEN` prototype input, behave as before.
8. **Capabilities.** `capabilities` reports that provider profiles can record
   an endpoint.

This amends ADR-0047. Its decisions stand, except that a selected profile must
now record an endpoint, and its consequence "bound to a provider kind, not to
an endpoint" no longer holds for a usable profile.

## Alternatives considered

- **Keep the endpoint on the command line and compare it with the profile.**
  Rejected: two sources for one value, and a mismatch rule to explain. The
  profile is the single source when one is selected.
- **Allow a profile without an endpoint to keep working with `--endpoint`.**
  Rejected: every profile created before this change would keep the old limit
  indefinitely. Requiring the endpoint removes it. ADR-0047's behaviour was
  never merged or released.
- **A command to change a profile's endpoint.** Rejected: it would need to
  unbind the credential in the same step to stay safe, which is what remove
  and add already do.
- **Bind the endpoint to the credential record, not the profile.** Rejected:
  a `none` profile has no credential but still needs a destination, and one
  record per profile is simpler to inspect.
- **Accept `https` endpoints for cloud kinds now.** Deferred: no adapter can
  exercise them, and the rule for certificates, redirects, and hosts belongs
  with the first adapter that needs it.
- **Normalise the endpoint before storing it.** Rejected: the validator
  accepts one narrow shape. The exact validated text is stored and compared.

## Consequences

- A stored credential is sent only to the endpoint recorded with its profile.
  Changing the destination means a new profile and a new credential.
- A profile recorded before this change has no endpoint and cannot be
  selected. The error names the cause. The operator removes it and adds it
  again with `--endpoint`, then stores the credential again.
- A control journal that contains an endpoint cannot be read by 0.30.0 or
  earlier, which reject the unknown field. Journals without one are unchanged.
- The stored-credential tests from ADR-0047 change: their profiles now record
  an endpoint, and the cases that passed `--endpoint` beside a profile become
  refusals.
- A desktop turn can now fail because a profile was replaced while the shell
  was running. The operator restarts the shell.
- Still open: endpoint rules for non-loopback providers, HTTPS and certificate
  handling, and any cloud adapter. A `none` profile is the only way to use a
  selected profile on a host without a protected store.

## References

- [ADR-0038](0038-record-provider-profiles-without-credentials.md)
- [ADR-0042](0042-store-provider-credentials-in-protected-os-storage.md)
- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [ADR-0047](0047-read-the-local-provider-token-from-the-credential-store.md)
- [Provider authentication](../provider-authentication.md)
