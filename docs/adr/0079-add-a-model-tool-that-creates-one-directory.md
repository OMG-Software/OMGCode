# 0079: Add a model tool that creates one directory

- Status: Accepted
- Date: 2026-11-23
- Amends: [ADR-0054](0054-add-multi-edit-and-create-model-file-tools.md)

## Context

`workspace_create_file` refuses a path whose parent directory does not exist,
and no model tool can create a directory. A model therefore cannot scaffold a
project from an empty workspace. In a recorded desktop session, a model
created `Cargo.toml` and then failed to create `src/lib.rs`, `src/main.rs`,
and `tests/integration.rs`. Each refusal was the recoverable code `not_found`,
which the model read as "the tool is unavailable" and stopped.

The file broker already creates one directory exclusively. Only the trusted
`file mkdir` command can reach it. Recursive directory creation is not
available ([file access](../file-access.md)).

## Decision

Add the model tool `workspace_create_directory`. Its only argument is `path`,
a workspace-relative path without parent segments. It creates exactly one
directory through the broker's exclusive single-directory creation. It never
replaces an entry, creates no missing parents, and takes no checkpoint, like
`workspace_create_file`.

Before any approval is recorded, an existing entry at the path is refused as
`file_exists` and a missing parent as `parent_not_found`. The tool is offered
under the same live preferences as the five file-write tools. It has its own
limit, `create_directory`, which can only narrow `write`. Under `prompt` each
call needs a trusted single-use approval, whose digest uses the domain
`omgcode-directory-create-v1`. Under `allow` it runs without one. The
approvals window shows the directory path. The model action journal records
it as `create_directory`. The tool schema version is `0.6.0`.

Stored settings that predate the limit have no `create_directory` value. Such
a store takes the stored `create` limit, so an operator who narrowed file
creation also narrows directory creation. The CLI sets the limit with
`workspace permissions --create-directory-tool`, and the desktop settings
window shows **Create directory tool**.

A missing parent is now reported as `parent_not_found` by
`workspace_create_file`, by the destination check of `workspace_rename_file`,
and by the new tool. Their descriptions tell the model to create the parent
with `workspace_create_directory` first. A missing source file is still
`not_found`.

## Alternatives considered

Letting `workspace_create_file` create missing parents would need one approval
to cover several directories and would reverse the documented refusal of
recursive creation. Each directory would also have to be shown in the
approval. One directory per call keeps each change visible and reuses the
existing broker operation.

Returning only the clearer `parent_not_found` code would explain the failure
but still leave the model unable to create the project.

## Consequences

A model can build a project tree one directory at a time. Every directory
creation is an approved or allowed write, recorded like other model actions.
No dedicated model tool removes a directory, so an unwanted empty directory
needs the operator or a command whose approved profile exposes a program that
removes it. A model needs one call per directory level, which uses tool-call
budget ([ADR-0076](0076-configure-the-tool-call-budget-per-turn.md)). Tool
definitions sent with each request grow by one entry.

## References

- [ADR-0054](0054-add-multi-edit-and-create-model-file-tools.md): the create
  tool and its prechecks.
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md):
  per-tool limits, approvals, and the model action journal.
- [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md):
  exclusive single-directory creation in the broker.
