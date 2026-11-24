# Skills and settings

This document defines the skills manager. Version 0.39.0 implements its first
increment (Phase F1, [ADR-0057](adr/0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md)):
trusted CLI import into private storage, a registry in the control journal
with application and workspace enablement, and loading of an enabled skill
into one chat turn on explicit `chat --skill` selection. The rest of this
document remains planned unless the [implemented subset](#implemented-subset-phase-f1)
names it. Skills are reusable task instructions and resources. They are not
permission grants or trusted application extensions.

## Implemented subset (Phase F1)

- **Commands.** `omgcode skills add <folder> --revision N`, `list`, `show`,
  `enable`, `disable`, `inherit`, and `remove`. All are trusted CLI commands
  that print JSON. `show` prints the manifest, digests, sizes, source, settings,
  and the `SKILL.md` body as a JSON string. There is no desktop control.
- **Import.** The folder is read through the native broker with a pinned
  directory identity. A link, junction, reparse point, special file, multi-link
  file, hidden or non-portable name (`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, no
  trailing dot, no Windows device name), or a case-insensitive name collision
  refuses the whole import. The folder's name must equal the skill `name`.
  Bounds: 128 files, 64 directories, depth 4, 128 entries per directory,
  1 MiB per file, 4 MiB in total, 64 KiB for `SKILL.md`, 8 KiB of frontmatter,
  and 32 KiB of body. Each file is read once; the same bytes are hashed and
  copied create-only into `<data_root>/skills-v1/packages/<skill_id>/`, which
  needs the same private data root as checkpoint storage. Empty directories
  are not copied. Nothing is executed and the source folder is never changed.
- **Format subset.** `saphyr-parser` supplies YAML events; OMGCode accepts one
  document with a block mapping of `name`, `description`, `license`,
  `compatibility`, `metadata` (a flat block mapping of scalars, at most 16
  entries), and `allowed-tools`. Aliases, anchors, tags, sequences, flow
  collections, other nesting, extra documents, duplicate keys, unknown keys,
  YAML directives, a byte order mark anywhere before the body, and NUL are
  refused. Scalars are literal text. A plain `null`, `Null`, `NULL`, `~`, or
  empty value makes an optional field absent; quoted text stays literal.
  `allowed-tools` is at most 1,024 bytes (`allowed_tools_too_long`); the
  other text limits count characters, as the specification does. It is
  stored and shown as advisory text and neither narrows nor widens any tool.
- **Registry.** `skill_added`, `skill_removed`, and `skill_enablement_set`
  records in the control journal, each revisioned and revalidated on replay.
  At most 64 skills, one per name; updating means removing and adding again.
  A new skill is disabled. A workspace override beats the application setting,
  and a skill with neither is disabled. `remove` records the removal first,
  then deletes the managed copy; a copy it cannot delete is reported and is
  inert. Each registered skill reserves journal room for its removal, so
  `remove` succeeds even when other changes have filled the journal. File
  reads and writes are refused once a permission withdrawal would no longer
  fit beside the grant and skill reserves. When the
  registration write's outcome is uncertain, `add` keeps the copy and reports
  `registration_uncertain` with its identifier; `skills list` shows whether
  it was recorded. An import never deletes a package directory it did not
  create. `list` and `show` refuse an unregistered `--workspace`. Unregistering a workspace
  drops its overrides in the same event (ADR-0081).
- **Loading.** `omgcode chat --skill <name>` selects up to four skills for one
  turn. Each must be registered and effectively enabled for the session's
  workspace, and its stored `SKILL.md` must match the recorded size and
  digest. The body is sent as one labelled, untrusted user-level message after
  the base prompt and saved preferences and before the history; the request
  manifest records each skill's identifier, name, and digest. Only a version 3
  session can select skills (`skill_session_version` otherwise). Skill messages
  are never added to the session history, replayed, or summarized; the
  recorded model reply can still repeat their text. Every turn rechecks the
  selection after compaction and before its first request. A tool-enabled turn
  also rechecks before each later model request and, under the settings lock
  that admits the operation, before each tool dispatch. The turn ends with
  `skill_withdrawn` when a selected skill's registry record was removed or
  replaced, its recorded digest changed, or it was disabled, including a
  disable followed by a re-enable: each skill's enablement changes are
  counted and compared with the count at load. The recheck reads the
  registry, not the stored copy. The turn keeps the body it verified at
  load, so deleting or editing the stored `SKILL.md` affects later loads
  only. The context estimate counts each skill message with its per-message
  overhead.
- **Not implemented:** Git sources, updates that keep the old version,
  session overrides, agent selection, resource loading, script execution,
  desktop controls, a settings page, cache invalidation beyond the
  request-only rule, and observing the end of dependent work: a command that
  is already running keeps running when a skill selected for its turn is
  withdrawn, and the runner's own admission step does not recheck skills.
  Each recheck is the admission point: a withdrawal after a check passes
  applies to the next request or dispatch, because the settings lock is not
  held for a whole model request or command.

## Bundled source packages

The [source catalog](../skills/README.md) includes the user-provided
[pull-request-review package](../skills/pull-request-review/SKILL.md).
Its instructions are preserved as package content. OMGCode does not register,
enable, or load it automatically; an operator can import it with
`skills add`. The catalog records provenance and a content digest; it does not
replace the core-owned registry.

## Settings page and scope

Provide Add, Remove, Enable, and Disable controls. Each entry shows its name,
description, source, pinned revision/version, content digest, compatibility,
requested tools, enabled scope, and validation status. Show its instructions
and resource list before enablement. Render all imported text safely.

Store skill packages in application-managed, per-user storage outside the
task-writable workspace. Maintain a separate core-owned registry of enablement.
Support application defaults, workspace overrides, and session overrides, with
the narrower scope taking precedence. Show the effective set and any override.
A skill disabled in the effective set must not be selected or loaded.

Adding imports a validated immutable copy and leaves it disabled until the user
enables it. Initial sources are a selected local folder or an explicitly selected
Git source pinned to an immutable commit. Fetching a remote source requires its
own permitted network operation. Neither import nor update runs scripts,
installs dependencies, enables the package, or follows a mutable latest version.
Updates require review of source/content changes and explicit user enablement
of the new version. Keep the old pinned version until that replacement is chosen.

Remove disables the registration and deletes its managed package when no active
reader needs it. It must not delete the user's original folder or Git repository.
Keep minimal audit identifiers under the history policy, not an executable copy
of a removed package. Do not promise to erase content already sent to a provider.

## Package format and validation

Adopt the [Agent Skills format](https://agentskills.io/specification) for the
initial package profile: a directory containing SKILL.md with YAML frontmatter
and a Markdown body. Validate the specification's required name and description,
field types/limits, naming rules, and directory-name match. Scripts, references,
and assets are optional. Preserve license and compatibility metadata.

OMGCode may reject unsafe or unsupported packages even when their metadata
is format-valid. Bound file count, total bytes, individual file sizes, parser
nesting, reference depth, and resource loading. Reject duplicate or ambiguous
metadata keys and unsafe YAML types. Do not fetch remote references implicitly.

For imports and resource reads, reject traversal, absolute paths, escaping
symlinks/junctions, unsafe hard links, and path-swap races. Copy authorized file
content into owned storage rather than retaining writable links to the source.
Apply the same rules to Git metadata, checkout behaviour, and any future archive
support. Validate content before publishing a registry entry. An interrupted
import must not leave a partially enabled package.

The format's experimental allowed-tools field is advisory metadata. It may
narrow tool selection within current policy. It cannot pre-approve execution,
change a permission switch, install a tool, or grant network or filesystem access.
Unsupported requirements must produce a visible compatibility result.

## Loading and execution

Load only minimal catalog metadata for effectively enabled skills. Load a
relevant skill body on explicit user selection or agent selection from that
enabled set. Load bounded references and assets only when needed. Record which
content was actually selected in the request manifest and show it to the user.
Repository content cannot register or enable a skill automatically.

Keep skill text source-labelled and below the base prompt and explicit user task.
Do not promote it into trusted custom instructions. If it asks to ignore policy,
reveal credentials, expand scope, or run a hidden command, treat that request as
untrusted content. Enabling a skill is consent to make its content available;
it is not approval of every action the content proposes.

Scripts and helpers use the ordinary [controlled runner](development-tools.md),
with the effective toolchain profile and current operation permissions. Apply
the same containment, limits, descendant cleanup, and output handling as any
other command. Package downloads and installations need their own authorization.
Do not use an import hook, host plugin API, or unsupervised interpreter path.

## Disable, removal, and active sessions

Apply enablement changes before the next request. A request already in flight
must be visibly associated with its old skill manifest. Disabling or removing a
skill blocks new loads and dependent tool dispatch. Stop and observe completion
of active tasks attributed to that skill before reporting the change complete.
Bind model-generated tool requests and their child tasks to the request's skill
manifest. Conservatively treat them as dependent on every selected skill when
finer attribution cannot be established. Do not trust model-declared attribution.

Invalidate derived context and cache entries that depend on changed or removed
skill content. Rebuild the next context without disabled skill bodies/resources
or summaries that carry their instructions. Preserve factual task outcomes as
labelled history where appropriate, using original evidence instead of blindly
retaining an old summary. Check old model responses against the current registry
before dispatch. Already-seen text cannot be made unseen; core authorization
must remain effective regardless of what the model remembers.

## Acceptance requirements

Test valid and malformed packages; duplicate metadata; name collisions; source
updates; large/deep resources; link/traversal/race attacks; interrupted import;
safe rendering; effective-scope overrides; disabled selection; lazy loading;
allowed-tools escalation attempts; script confinement; disable during streaming
and execution; cache/summary invalidation; and removal without deleting originals.
Document the supported format subset and verify it on Windows and Linux.
