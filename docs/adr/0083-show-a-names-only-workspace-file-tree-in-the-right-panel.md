# 0083: Show a names-only workspace file tree in the right-hand panel

- Status: Accepted
- Date: 2026-11-23
- Amends: [ADR-0072](0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md) decisions 3 to 5

## Context

The operator asked that the right-hand column, until now one terminal
placeholder, hold the terminal as one tab beside tabs for the workspace's
file tree, Git status, background tasks, and a context breakdown. This record
covers the tab framework, the Files tab, and a fixed footer slot that stays
visible on every tab. The other tabs and the footer's content are later work.

Constraints:

- The webview has no authority. Rust authorizes every command, and no command
  takes a host path ([ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)).
- File names in a workspace are untrusted text. The frontend renders them only
  as text nodes.
- The model's list tool is a policy-checked, audited operation that needs the
  workspace's `read` preference. A tree the operator opens to look at their own
  workspace is a different act, but it still must not widen what the webview
  can do.
- The broker's `.OMGCode` store and its link refusals
  ([ADR-0070](0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md))
  must hold for any caller.

## Decision

1. **A tab framework.** `src/main/side_panel.ts` builds the right-hand panel
   from a list of tab entries (id, label, rail icon, content, optional
   visibility callback). It reuses `src/ui/tabs.ts`, so keys follow the
   WAI-ARIA tabs pattern. Collapsed, the rail has one icon per tab, and an icon
   expands the panel on that tab. The selected tab lasts until the window
   closes. Another tab is one more entry in the list.
2. **A fixed footer slot.** The panel has a footer under its scrolling content
   area, visible on every tab, empty for now, and exposed as `footer` in the
   panel's API.
3. **The Terminal tab is unchanged in meaning.** It keeps ADR-0072's note and
   calls no command. The panel's id, class, and 360-pixel width are kept so the
   layout, separators, and their tests keep their meaning; the separator and
   collapse button now say "side panel".
4. **A Files tab and `workspace_entries`.** A new `main` command
   `workspace_entries { session_id, path }` lists ONE directory.
   - Rust resolves the workspace from the session's recorded workspace and the
     settings store. It never takes a path from the webview. `path` is an array
     of relative name components. Each must be non-empty, not `.` or `..`, free
     of `/`, `\`, and NUL, not the reserved `.OMGCode` name in any case, and at
     most 255 characters; at most 32 components. The broker validates the
     joined path again for the host.
   - The listing goes through the platform broker's directory handle on the
     registered development root and workspace, with the same creation-time
     identity check as the Git branch read ([ADR-0064](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)).
     No link is followed out of the workspace. The `.OMGCode` store is refused
     as a path and hidden from a listing.
   - It returns names and kinds (`directory`, `file`, `other`) and a
     `truncated` flag, directories first and then by name, at most 512 entries
     (the broker's bound, applied before sorting, so a truncated directory
     shows a subset chosen by the file system). The broker reports a link,
     reparse point, or special entry as one blocked kind, so `other` stands for
     all three. A name that is not valid Unicode is returned lossily with
     `lossy` set, and the frontend never sends it back. No content, size, or
     date is returned.
   - **Security decision.** The operator's own view does not require the
     workspace's `read` preference, is not a model action, is not journaled,
     and grants nothing: a listing never authorizes a read, edit, or run, and
     the model's tools are unchanged. It requires a registered workspace and
     refuses an unregistered one with `unknown_workspace`. It lists only inside
     the open session's workspace.
   - **What the webview learns.** The `main` webview can now learn the file and
     folder names of the open session's workspace, even when `read` is `deny`.
     This is acceptable because the operator registered that workspace and
     opened that session, the webview already renders the session's transcript
     (which names the same files), names carry no content, and the same
     operator can read the folder in any file manager. A compromised `main`
     webview could still not read, change, or run anything in it.
5. **The frontend** lists lazily, only while the tab is visible, clears on a
   session change, ignores a reply for a session that is no longer open, draws
   at most 5,000 rows, and shows empty and error states as text.

## Alternatives considered

- **Reuse the audited model list operation.** Rejected: it needs the `read`
  preference and writes a journal record per listing, so the tree would fail
  for a workspace whose `read` is `deny` and would flood the audit log with the
  operator's clicks.
- **Take a host path or a workspace identifier from the webview.** Rejected: the
  command contract gives the webview no path. The session already records its
  workspace.
- **List recursively in one call.** Rejected: an unbounded tree. One bounded
  directory per call keeps the cost and the exposure small.
- **Return file sizes and dates.** Rejected for this increment: names suffice
  and each metadata item is more to review.
- **A separate window or panel for files.** Rejected: the operator asked for
  tabs in the right-hand column.
- **Build a new tablist.** Rejected: `src/ui/tabs.ts` already follows the
  pattern.

## Consequences

- The `main` window gains a 43rd command and a Files tab. The model tools,
  approvals, and workspace preferences do not change.
- The tree shows at most 512 names per directory, and which names are left out
  of a larger one depends on the file system's order.
- Links and special entries are not distinguished or opened.
- The tree does not refresh by itself; **Refresh** or reopening a folder lists
  again.
- Linux has the same code path through the broker but the Files tab has not
  been driven there; the Rust tests ran on the Windows development host.

## References

- [Desktop interface](../desktop-interface.md#right-hand-panel)
- [Workspace file access](../file-access.md)
- [ADR-0072](0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md)
