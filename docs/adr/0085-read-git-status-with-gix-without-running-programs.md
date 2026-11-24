# 0085: Read Git status with gix without running programs

- Status: Accepted
- Date: 2026-11-24
- Builds on: [ADR-0064](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)
  (the branch is read from `HEAD` with no Git process)

## Context

The operator asked for a **Git status** tab in the `main` window's right-hand
panel: the open session's workspace branch and its changed files. The branch
reader of ADR-0064 reads one file, `HEAD`, and cannot say which files changed.
Changed files need the index, the object database, ignore rules, and a walk of
the worktree.

The threat is that a workspace's Git metadata is attacker-writable. A model
with `write` access can edit `.git/config` and `.gitattributes`. Git, and any
library that copies its behaviour, runs programs that this configuration names:

- `filter.<name>.clean`, `filter.<name>.smudge`, and `filter.<name>.process`,
  selected by a `filter=<name>` attribute, run while content is compared;
- `diff.<name>.textconv` and `diff.external` run for content comparison;
- `core.fsmonitor` names a hook that `git status` runs to learn which files
  changed;
- `core.hooksPath` and the hooks in `.git/hooks` run for other commands.

The desktop process holds the operator's authority, not the workspace
sandbox's. Running one of these programs from it would escape the sandbox that
the `run` command applies. The operator decided: use the pure-Rust `gix` crate
and never run an external program.

## Decision

1. **Library.** `gix` 0.91.0 is a workspace dependency of `omgcode-core` with
   `default-features = false` and features `status` and `sha1`, pinned with
   `=`. It adds no network client, HTTP, TLS, credential-helper, or
   `fsmonitor` feature, no `max-performance` C dependency, and no `zlib`
   C backend. The lockfile gained 81 packages net (82 added, one `syn` patch
   version replaced); they are listed in the pull request. Two of them,
   `gix-transport` and `gix-protocol`, are compiled because `gix` depends on
   them unconditionally; with no client feature they contain no connection
   code. `gix-command` is compiled because `gix-filter` depends on it; the
   next decision keeps it from running anything.

2. **Isolated open.** `omgcode_core::git_status` opens the repository with
   `gix::open::Options::isolated()`: no system, global, user, or
   environment configuration, no `include` or `includeIf`, no Git
   installation lookup, and no environment variables. It opens exactly the
   workspace folder; it does not search parent folders.

3. **No repository-controlled command.** The open options carry a section
   filter that rejects every section from the repository's own `.git/config`
   and worktree configuration. `gix` builds its filter drivers and diff
   drivers only from accepted sections, so the status run knows no
   `filter.<name>.*` or `diff.<name>.*` command, and a `filter=<name>`
   attribute names no driver. Built-in conversions (line endings, `text`,
   `ident`) stay in process. The two controls overlap for included files.
   In `gix` 0.91.0, isolated permissions set the `includes` permission to
   false, so `include` and `includeIf` are not followed, and `gix-config`
   0.63.0 gives a followed include the including file's source, so the filter
   would still reject its sections as `Local`. Because `gix` is built without `fsmonitor`,
   `core.fsmonitor` is read by nothing. No hook runs: the status run does not
   call any. Submodules are ignored, so no second repository is opened.

4. **Same boundary as the branch.** The command takes only a session id. Rust
   resolves the session's recorded, registered workspace, opens it by its
   recorded identities through the broker (the same `open_scope` the branch
   reader now shares), and requires `.git/HEAD` to be readable through that
   anchored handle, so `.git` is a plain folder inside the workspace. A
   `.git` file (a linked worktree or submodule pointer) is reported as
   unavailable and not followed. When `.git/HEAD` cannot be read, the same
   handle names what `.git` is, without following it: missing
   (`not_repository`), a pointer file, a folder whose `HEAD` is missing or
   unreadable, or a link or special entry, each with its own unavailable
   reason. After the open, the repository's worktree
   and Git folder must be the workspace and its `.git` by canonical path, so
   a `core.worktree` or alternative layout is refused. An unregistered
   workspace returns the `unknown_workspace` error. A folder with no `.git`
   is the normal `not_repository` state.

5. **Bounds.** At most 1,000 changed paths are returned, with a `truncated`
   flag; the list stops at the bound. Without its `parallel` feature `gix`
   collects the whole result before the list is cut, so memory is bounded
   only by the repository and the deadline below. A watchdog interrupts the run
   after 10 seconds and reports "reading the status took too long".
   Untracked folders collapse to one entry. Rename detection between `HEAD`
   and the index considers at most 100 renames. Paths are lossy text for
   display, made display-safe in the app crate, and never reused as paths.
   The command runs on the blocking pool, not the async executor. The
   service takes the workspace's recorded scope and branch with the control
   store open, then releases that store, which is the settings lock, before
   the status run, so a run of up to 10 seconds does not refuse a send or a
   settings change as busy. The run still opens the workspace by its recorded
   identities; a workspace unregistered between the two steps is read once
   more for display, which grants nothing.

6. **Read-only.** The status run does not write the index, refresh stat data,
   or create any file. The tab shows data and sends nothing back.

7. **Tests.** The key test builds a temporary repository with `gix`, sets
   `filter.evil.clean`, `smudge`, and `process`, `core.fsmonitor`,
   `core.hooksPath`, `diff.external`, and a `diff.evil` driver to a command
   that writes a marker file, adds `* filter=evil diff=evil` to
   `.gitattributes`, changes and adds files, runs the status function, and
   asserts the marker was never created. It runs on Windows and Linux. A
   control run, made once by hand with the section filter removed, created the
   marker, so the test can fail. A second test puts the same commands only in
   a file that `.git/config` names with `include` and `includeIf`. Control
   runs made by hand showed it creates the marker when includes are followed
   and the filter is removed, and stays clean with includes followed and the
   filter kept. Other tests cover a clean repository,
   worktree and staged changes, collapsed untracked folders, the 1,000-path
   bound, a `core.worktree` that points elsewhere, a `.git` folder that is not
   a repository, a corrupt index, a `.git` folder without `HEAD`, and a `.git`
   pointer file; the last four are controlled `unavailable` values.

## What `gix` still reads

The repository's `.git/config` is parsed, but only values the status needs and
no section that selects a program is honoured. The index, `HEAD`, refs, loose
objects, packs, `.gitignore` and `.gitattributes` files in the workspace,
`.git/info/exclude`, and `.git/info/attributes` are read. A `.git/config` that
sets `core.excludesFile` or `core.attributesFile`, or an
`objects/info/alternates` entry, can name a file outside the workspace. That
file is read, never run or shown; it can change which paths are listed. This
is a disclosure of at most one bit per path to the operator's own window, and
is accepted. Reading is not bounded by the sandbox, so a very large repository
or a hostile pack can cost time and memory, which the deadline and path bound
limit. `gix` is a large dependency tree that has not been audited here.

## Alternatives considered

- **System `git status` with `-c` flags.** Disabling the keys on the command
  line (`-c core.fsmonitor=false`, and so on) cannot name every
  `filter.<name>` the workspace may define, and a Git version can add a new
  key that runs a program. It also needs an installed Git. Rejected: it runs
  a program under the operator's authority against attacker-written files,
  and its safety rests on a list that must stay complete.
- **Branch only, from `HEAD`.** Safe, and already built (ADR-0064), but it
  does not answer the feedback. Kept as the branch line of the tab.
- **`git2`/libgit2.** It links a C library, and its configuration search and
  filter handling are not under this project's control. Rejected for the C
  dependency and the weaker control over configuration.
- **A hand-written index and ignore reader.** Smaller, but a custom
  implementation of ignore rules, attributes, and index formats is a worse
  source of correctness and security defects than a maintained library.
- **Run Git inside the existing sandboxed runner.** The runner is for the
  operator's declared toolchain on a workspace whose `execute` preference is
  enabled; status display must work without it and without a profile.

## Consequences

- The Git status tab can show changed files without a Git installation and
  without running any program.
- `omgcode-core` now compiles `gix` and its dependency graph; build time and
  the lockfile grow.
- Linked worktrees and submodule checkouts show "unavailable". Submodule
  status and ignored files are not shown.
- Settings that `gix` reads through the section filter are ignored, because
  the repository's own sections are rejected. A repository whose status
  depends on one of them (for example `core.safecrlf`) can list a file as
  changed that Git would not.
- A new `gix` release can change what configuration it reads. The pin and the
  marker test are the guard; update both together.

## References

- [Desktop interface](../desktop-interface.md#git-status-tab)
- [ADR-0064](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)
- [Development: Dependencies](../development.md#dependencies)
