# 0100: List the recorded toolchain profiles in the command tool

- Status: Accepted
- Date: 2026-11-26

## Context

The session described in ADR-0099 continued on a 0.8.0 tool schema build.
That build created the file the earlier turn could not. The model then asked
`workspace_run_command` to run `cargo fmt --check` under the profile `rust`.
The operator approved the call in the desktop approvals window. The run
failed with `command_failed` and `workspace control failed: toolchain profile
is not recorded`. The operator's settings journal had never recorded a
toolchain profile.

The causes are in the core:

- The runtime offered the command tool whenever `model_tools`, `execute`,
  `read`, and `write` permitted it. Every run needs a recorded profile and has
  no unconfined fallback (ADR-0030), so with no profile every call was bound
  to fail.
- The `profile` parameter was described only as "Recorded toolchain profile
  name". The model could not know which names exist, so it guessed.
- `dispatch_command` requested or consumed an approval before anything looked
  the profile up. Only the runner did, after the operator had decided. The
  operator was asked to approve a command that could never start.

## Decision

- **List the profiles.** `command_tool_definition` takes the names of the
  profiles recorded for the running platform, in the order they were added,
  and puts them in the `profile` parameter as a JSON Schema `enum`. The turn
  reads them from the same settings snapshot that selects the tools. A profile
  declared on another platform is left out, because the runner refuses it.
- **Withhold the tool without a profile.** The command tool is advertised
  only when the preferences permit it and at least one profile is recorded
  for the running platform. An empty `enum` would be an unsatisfiable schema,
  and every call would fail.
- **Refuse an unknown profile before any approval.** Under the settings lock
  that admits the call, before any approval is matched, requested, or
  consumed and before a standing-allow run, a profile that is not recorded
  for this platform is answered with `profile_not_recorded` and the recorded
  names. Names are compared case-insensitively, as the runner compares them.
  Nothing runs and no approval is recorded. The model action journal records
  the call as refused with reason `invalid_request`, so its schema does not
  change. The turn continues. The runner still looks the profile up under its
  own lock, so a profile removed in between still refuses the run.
- **Schema version.** `TOOL_SCHEMA_VERSION` becomes 0.9.0. Journals that
  recorded earlier versions replay unchanged.

A profile name has at most 64 ASCII letters, digits, `.`, `-`, or `_`, and
the operator records it through the trusted CLI or the `settings` window, so
it cannot carry instructions to the model. Listing a name grants nothing. The
approval, standing-allow, and runner checks are unchanged.

## Alternatives considered

- **Keep offering the tool with no profile and let the runner fail.** This is
  the behaviour that wasted the operator's approval and the model's call.
- **Offer the tool without an `enum` and explain only in the result.** The
  model would still guess on its first call, and the description is a static
  string shared by every turn.
- **Put the names in the tool description.** That would make the description
  vary per turn. The `enum` constrains the value in the schema, where
  providers that honour schemas can enforce it, and the dispatch check covers
  providers that do not. The plan route sends tools with `strict: false`.
- **List each profile's roots, `PATH` entries, or variables.** The model needs
  only a name to choose. Paths and variable values are operator configuration
  that the model has no use for.
- **Add a journal refusal reason such as `profile_not_recorded`.** A new
  variant would stop older builds from reading the action journal.
  `invalid_request` already means a request that failed validation.

## Consequences

- A workspace whose operator has recorded no profile is no longer offered
  the command tool. The model can only say that it has none. The operator
  must add a profile through `omgcode workspace profile-add` or the
  `settings` window. Nothing discovers toolchains yet; see the
  [development-tool contract](../development-tools.md#tool-sources).
- With up to 32 profiles of 64-character names, the command tool's schema
  grows by about 2.1 KiB. The desktop tool-schema estimate rises to cover
  that worst case, and a unit test keeps it above the measured size.
- The approval window no longer shows a command that names an unrecorded
  profile.
- The model can still name a recorded profile that lacks the program, or that
  has no Windows container grant. Those calls fail in the runner as before.

## References

- [Execution contract: model command tool](../execution.md#model-command-tool)
- [Execution contract: toolchain profiles](../execution.md#toolchain-profiles)
- ADR-0030, ADR-0040, ADR-0063, ADR-0088, ADR-0099
