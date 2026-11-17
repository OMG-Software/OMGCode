# 0056: Send registry credentials to one declared origin

- Status: Accepted
- Date: 2026-11-17

## Context

The trusted `packages fetch` command (ADR-0035) sends no credentials, so it
cannot reach a private registry. ADR-0035 left credentials to a separate
design and ADR-0034 forbids secrets in profiles, run environments, journals,
and model context. ADR-0042 stores provider secrets in protected OS storage
and binds them by opaque identifier, and ADR-0047 and ADR-0048 tie a stored
credential to the one destination it was recorded for.

A package-network profile may mix a registry with a download host such as a
CDN. A credential that suits the registry must not reach the CDN. The fetch
refuses redirects, so a response cannot move a credential to another origin,
but the profile itself can name several.

## Decision

An operator can bind one stored secret to one declared origin of a
package-network profile. Only the trusted `packages fetch` reads it.

- **Binding.** The control journal records
  `package_credential_bound { profile_id, origin, auth, binding }` and
  `package_credential_removed { profile_id, origin, credential_id }`. `origin`
  is the canonical text of one of the profile's stored origins
  (`scheme://host:port/prefix/`, lowercase host, explicit port), used both
  when recording and when matching. `auth` is `bearer` or `basic`. `binding`
  is the identifier and backend of ADR-0042. The journal never holds a secret.
  `package_profile_removed` gains an optional `credential_ids` list, so
  removing a profile ends every binding in the same event, as provider profile
  removal does. Journals written earlier still replay.
- **Replay rules.** The profile must exist and the origin must be one of its
  origins. An origin holds at most one credential. A credential identifier is
  bound at most once across all package and provider bindings, and rebinding an
  origin to the same identifier is invalid. A removal must name the currently
  bound identifier. Binding and removal advance the settings revision and need
  the expected revision.
- **Store and rotate.** The order follows `set_provider_credential`: store the
  new native secret, record the binding, then delete the replaced secret. A
  refusal before the journal write deletes the new item; an uncertain journal
  write keeps it and reports its identifier. The shared helpers
  (`store_failure`, `binding_write_uncertain`, `delete_unbound_credential`)
  serve both kinds of binding.
- **Secret validation.** Before anything is stored: at most 16 KiB, visible
  ASCII, no control character or line break, and no leading or trailing
  whitespace. A bearer token has no whitespace at all. A basic secret is
  `user:password` with a colon and a nonempty user.
- **Fetch.** The matched origin decides. When several origins match the URL,
  the longest path prefix wins, so a credential bound to a broad origin is not
  sent to a path a narrower origin declares. If the matched origin has a
  binding, the fetch opens the credential store, loads the secret under the
  still-open control store, builds one `Authorization` header (`Bearer
  <secret>` or `Basic <base64(user:password)>`), and drops the control store
  before any network I/O. The header value is marked sensitive and built in
  zeroizing buffers. If the store cannot be opened or the secret cannot be
  read, the fetch fails with a typed error and sends nothing. It never falls
  back to an unauthenticated request or to another credential. Like every
  refusal before the request, this failure happens before the start record is
  written, so nothing is recorded. An origin without a binding behaves as
  before. Redirects stay refused, so the header cannot follow one.
- **Audit.** `package_fetch_started` carries the optional `credential_id` only
  when a credential was attached. The receipt reports the same identifier. The
  secret, the header, and the URL text are never recorded or printed.
- **Operator only.** `workspace package-credential-set` reads the secret from
  piped standard input only, refuses a terminal, and reads it before the
  control store opens. `package-credential-remove` unbinds, then deletes.
  `package-profile-remove` deletes the profile's bound secrets after its
  event. `package-profile-show` lists the bound origins with auth, identifier,
  and backend. `capabilities` reports `package_registry_credentials`. No model
  tool, run, or provider adapter can bind, read, or receive one.
- **Runs.** A confined run never receives a registry credential: no run
  environment variable, file, or journal field carries one.

## Alternatives considered

- **One credential per profile.** Rejected: a profile may mix a registry and a
  CDN, and one token would go to both.
- **A credential in the URL, a profile, or an environment variable.**
  Rejected by ADR-0035 and ADR-0034: URL text is not recorded, profiles hold
  no secret, and environment input leaks into process listings and children.
- **Giving runs the credential.** Rejected: no run-side destination control
  exists (ADR-0034), so a hostile tree could send it anywhere.
- **Fall back to an unauthenticated request when the secret is unreadable.**
  Rejected: a silent downgrade can fetch a public file of the same name and
  hide a broken binding.
- **A new third-party base64 crate.** Rejected: the encoder is small, has the
  RFC 4648 vectors as tests, and avoids a new direct dependency.

## Consequences

- A private registry can be fetched without a secret in a journal, a
  command line, a run, or model context.
- The credential still crosses the network to the declared origin, and the
  registry can log it. Transport security is the declared origin's `https`;
  only a loopback test fixture may use `http`.
- `Bearer` and `Basic` are the only schemes. Tokens that need another header
  or a custom signing step are unsupported.
- The operator chooses the scheme. A wrong scheme fails at the registry.
- An older binary refuses a journal that records the new actions, because the
  action enum denies unknown events; a journal written with these events
  cannot be read by the previous version.
- A bound origin whose secret was deleted outside OMGCode refuses every fetch
  from it until the operator rebinds or removes the credential.
- Memory zeroing is best effort: the HTTP stack keeps its own copy of the
  header. Any process running as the same user can read the stored secret
  (ADR-0042).
- The tests use loopback HTTP and the real native store. They do not qualify a
  live HTTPS registry.

## References

- [Command execution](../execution.md#trusted-package-fetch)
- [Security coding rules](../security-coding-rules.md)
- [Testing standards](../testing-standards.md)
- [ADR-0034](0034-gate-package-networking-behind-enforceable-destination-control.md)
- [ADR-0035](0035-fetch-packages-through-a-trusted-allowlisted-cache.md)
- [ADR-0037](0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md)
- [ADR-0042](0042-store-provider-credentials-in-protected-os-storage.md)
- [ADR-0047](0047-read-the-local-provider-token-from-the-credential-store.md)
- [ADR-0048](0048-bind-a-stored-credential-to-its-profile-endpoint.md)
