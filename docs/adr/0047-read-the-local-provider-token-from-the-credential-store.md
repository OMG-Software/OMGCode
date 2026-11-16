# 0047: Read the local provider token from the credential store

- Status: Accepted
- Date: 2026-11-16

## Context

ADR-0042 stores API keys in protected OS storage and binds each one to an
`api_key` provider profile. Nothing reads them. The only working model route
is the local LM Studio adapter, and its optional server token still comes from
the `OMGCODE_LM_TOKEN` process environment variable. The
[authentication contract](../provider-authentication.md) says that input does
not meet the credential-service requirement.

This decision makes the first adapter read a stored credential. It is the
smallest step of Phase D that changes where a real secret comes from.

These constraints shape it:

- **A key must reach only its own provider.** The contract forbids sending an
  OpenAI key or a subscription token to LM Studio. Profiles record a provider
  kind, so the kind can be checked.
- **No silent fallback.** A selected credential that cannot be read must stop
  the turn. It must not become a request without a token, or with another
  token.
- **A profile stores no endpoint.** The endpoint is still chosen on the command
  line and must be a literal loopback address.
- **The webview has no authority.** The desktop shell fixes its endpoint at
  launch (ADR-0045). The credential source must be fixed the same way.
- **Rotation is not atomic with a read.** `provider-credential-set` records the
  new binding and then deletes the old item.

## Decision

1. **Selection by profile name.** `omgcode chat` and `omgcode session compact`
   accept `--provider-profile <name>`. `omgcode-desktop` accepts the same flag
   at launch, for the whole process. The webview cannot choose or see it.
2. **Core decides whether a credential may be used.**
   `ControlStore::load_provider_credential` takes the profile name and the
   provider kind of the adapter that is asking. It refuses an unknown profile,
   a profile of another kind, a `subscription` profile, an `api_key` profile
   with no bound credential, and a binding whose backend is not the one this
   host opened. For a `none` profile it returns no credential. Only then does
   it load the secret. The checks that need no native store are in
   `provider_credential_binding`, which takes no credential store and so
   cannot touch native storage. The caller opens the native store only after
   that function returns a binding, so a `none` profile works on a host whose
   protected store is unavailable.
3. **Only the LM Studio adapter asks.** It passes the `lm_studio` kind, so a
   profile of any other kind is refused before any native store access. No
   other adapter exists.
4. **Fail closed.** With a profile selected, any refusal or store failure ends
   the turn with a `credential` error before a model request is built. A
   locked Secret Service is an error; OMGCode still never runs a prompt.
5. **The environment variable is not combined with a profile.** With a profile
   selected, `OMGCODE_LM_TOKEN` is not read. Without one, behaviour is
   unchanged: the variable remains the prototype input.
6. **Read under the settings lock, per turn.** The secret is loaded while the
   settings store is held, so a rotation cannot delete the item between the
   binding lookup and the load. The store is released before any network
   request. The desktop loads the credential again for each turn and keeps no
   copy between turns, so a rotation takes effect on the next turn.
7. **Reading records nothing.** A load is not a settings change and does not
   append to the control journal. The session journal is unchanged: it does
   not name the profile or the credential.
8. **Handling in memory.** The loaded token is held in a zeroing buffer with a
   redacted `Debug` and no serialization, for one turn. The existing
   redaction of the token from recorded and returned text applies to it. The
   provider client still holds its own copy in a request header for the turn.
9. **Startup check in the desktop.** With a profile selected, the shell
   resolves the credential once at launch and exits with status 1 if that
   fails, before any window opens. It keeps nothing from that check.
10. **Capabilities.** `capabilities` reports which adapters can read a stored
    credential. `provider_adapters` stays `false`: no adapter exists for any
    provider other than the local LM Studio route.

## Alternatives considered

- **Remove the environment variable now.** Rejected: it would break every
  existing workflow in one step, and a workspace without a settings store has
  no profile to select. It stays as the documented prototype input.
- **Use the profile's credential automatically when exactly one LM Studio
  profile exists.** Rejected: a credential would start being sent without the
  operator selecting it.
- **Prefer the environment variable when both are set, or merge them.**
  Rejected: two sources for one header is ambiguous. A selected profile is the
  only source.
- **Fall back to no token when the credential cannot be read.** Rejected: a
  request would reach a different authentication mode than the one selected.
- **Let the webview choose the profile.** Rejected: it would let page script
  decide which secret is sent.
- **Cache the token in the desktop service.** Rejected: it would lengthen the
  secret's lifetime and delay rotation. One native read per turn is cheap.
- **Record each use in the control journal.** Rejected: the journal has bounded
  capacity and a read changes no setting.
- **Add an endpoint to the profile and bind the credential to it.** Deferred:
  it changes the profile record format. It is the right next step before any
  non-loopback endpoint is allowed.

## Consequences

- A token no longer has to live in the process environment, where child
  processes and process listings can inherit or expose it.
- The credential is bound to a provider kind, not to an endpoint. A stored LM
  Studio token is sent to whichever literal loopback endpoint the operator
  selects. Loopback-only validation limits that to a local port.
- Any process running as the same user can still read the stored credential
  (ADR-0042). This change does not alter that.
- A turn started while another process holds the settings store fails with a
  busy error instead of waiting.
- A memory-only session credential, for a host whose protected store is
  unavailable, is still not implemented. Such a host must use a `none` profile
  or the environment variable.
- The provider client's header copy and the transport's buffers are not
  zeroed. Zeroing covers only the copy this layer holds.
- No cloud adapter, subscription sign-in, token refresh, or account lifecycle
  exists. Those remain Phase D work.

## References

- [ADR-0038](0038-record-provider-profiles-without-credentials.md)
- [ADR-0042](0042-store-provider-credentials-in-protected-os-storage.md)
- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [Provider authentication](../provider-authentication.md)
- [LM Studio server authentication](https://lmstudio.ai/docs/developer/core/authentication)
