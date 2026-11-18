# 0057: Import skills into a private registry and load them on explicit selection

- Status: Accepted
- Date: 2026-11-18

## Context

The [skills contract](../skills.md) plans a skills manager: validated,
immutable imports in application storage, a core-owned enablement registry
with application, workspace, and session scopes, lazy loading of enabled
skills, script execution through the controlled runner, and invalidation of
derived context when a skill changes. None of it existed. The contract is too
large for one increment.

The [Agent Skills specification](https://agentskills.io/specification),
checked on 2026-11-18, defines a package as a directory with `SKILL.md`: YAML
frontmatter with a required `name` (1 to 64 lowercase letters, digits, and
single inner hyphens, equal to the directory name) and `description` (1 to
1024 characters), optional `license`, `compatibility` (1 to 500 characters),
`metadata` (a string-to-string map), and the experimental `allowed-tools`
(a space-separated string), followed by a Markdown body. Other files are
optional; `scripts/`, `references/`, and `assets/` are conventions.

The user chose this first increment (F1): import, the registry, and loading
an enabled skill into a chat turn on explicit selection; a maintained YAML
parser with OMGCode validation over it; and copying every package file while
never running one.

## Decision

**Import.** The trusted `omgcode skills add <folder>` reads an operator-selected
folder through the platform broker with a pinned directory identity. Any
link, reparse point, special file, multi-link file, hidden or non-portable
name, or case-insensitive name collision refuses the whole import. Bounds:
128 files, 64 directories, depth 4, 1 MiB per file, 4 MiB in total, 64 KiB
for `SKILL.md`, 8 KiB of frontmatter, and 32 KiB of body. Each file is read
once and the same bytes are hashed and copied, create-only, into
`<data_root>/skills-v1/packages/<skill_id>/` in the verified private store.
The package digest is SHA-256 over a sorted list of each file's digest, size,
and path. Nothing in the package is executed, and the source folder is never
changed.

**Format.** `saphyr-parser` 0.3.4 supplies YAML events only. OMGCode accepts
one document holding a block mapping of the specification's fields, with
scalar values and a flat scalar `metadata` mapping. Aliases, anchors, tags,
sequences, other nesting, extra documents, duplicate keys, and unknown keys
are refused. Scalars are taken as literal text. `allowed-tools` is stored and
shown as advisory text; in F1 it neither narrows nor widens any tool.

**Registry.** The control journal records `skill_added`, `skill_removed`, and
`skill_enablement_set`. Each advances the settings revision and needs the
expected revision. Replay revalidates every field and bound. A name is
registered at most once; updating a skill means removing and adding it. A new
skill is disabled. A workspace override (`enabled` or `disabled`; `inherit`
removes it) takes precedence over the application setting, and a skill with
neither is disabled. Removing a skill or a workspace drops the settings that
name it. The journal, not the package directory, authorizes a load; a
package directory without a record is inert. `skills remove` records the
removal before it deletes the managed copy.

**Loading.** `omgcode chat --skill <name>` selects up to four skills for one
turn. Before the first model request, core requires each to be registered and
effectively enabled for the session's workspace, reads its stored `SKILL.md`,
and refuses a size or digest that differs from the record. Each body becomes
one user-level message, labelled with the skill name and digest as untrusted
skill text that cannot grant permissions, placed after the base prompt and
saved preferences and before the history. The request manifest records each
skill's identifier, name, and digest. Skill messages are not session history:
later turns, replay, and the compaction summarizer never receive them. In a
tool-enabled turn, core rechecks every selected skill before each later model
request and each tool dispatch, and ends the turn with `skill_withdrawn` when
one was removed, disabled, or changed.

## Alternatives considered

- **A hand-written frontmatter subset parser.** It needs no dependency, but
  the user chose a maintained YAML parser. Using it only at the event level
  keeps OMGCode's refusals explicit and avoids typed YAML values.
- **A separate skills journal.** It would not consume the control journal's
  4,096-event budget, but it would duplicate revision checks, locking, and
  workspace references. The registry is bounded at 64 skills instead.
- **Session-scope overrides and agent selection.** The contract plans both.
  Explicit per-turn selection gives the user's choice directly and avoids
  invalidation of persisted selections; both remain planned.
- **Keeping skill text in the session history.** Replaying it would carry a
  disabled skill into later turns and summaries. Request-only messages avoid
  that.
- **Skipping unsupported entries during import.** A silent skip changes the
  package without the operator noticing. The import is refused instead.

## Consequences

- An operator can review a skill (`skills show` prints its manifest, digests,
  file list, and body as JSON data) before enabling it, and enable it per
  workspace.
- A control journal with skill records and a session journal whose request
  manifest names skills are refused by 0.38.0 and earlier, as with every
  earlier new record kind. No schema version changed. Only a version 3
  session can record a skill selection; an older session refuses one.
- Scripts, references, and assets are stored but never loaded or run. Skill
  text loaded into a turn cannot be unseen; core authorization, approvals, and
  live preferences remain the only authority.
- Not implemented: Git sources, updates that keep the old version, session
  overrides, agent selection, resource loading, script execution, desktop
  controls, and cache invalidation beyond the request-only rule.

## References

- [Skills contract](../skills.md)
- [Security coding rules](../security-coding-rules.md)
- [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md)
- [ADR-0038](0038-record-provider-profiles-without-credentials.md)
- [ADR-0041](0041-compact-active-context-with-readable-summaries.md)
