# 0044: Observe journals without the writer lock and index them in SQLite

- Status: Accepted
- Date: 2026-11-16

## Context

A session tree must list every session with its title, folder, times, and
status, and the user selected full-text search over transcripts. Before this
decision the core could not support either:

- **One reader, and it locks.** `SessionJournal::open` was the only way to
  read a journal, and it takes the exclusive writer lock. On Windows that lock
  is mandatory, so a second handle cannot read a journal that a running chat
  holds. A scan that takes the lock would also make a writer in another process
  report `Busy`.
- **No times.** The session snapshot dropped every event timestamp.
- **No listing.** Nothing enumerated `sessions/`.
- **No index.** The [session storage contract](../session-storage.md) says a
  search or tree index is derived and rebuildable, and left its database
  format unselected.

The contract also requires that the frontend never chooses a raw journal path,
and that journals are validated as untrusted input when read.

## Decision

1. **Lock-free inspection.** `SessionInspection::inspect` reads one journal
   without calling any lock function. It opens read-only without following
   links, runs the same platform handle validation as the writer, reads within
   the 8 MiB journal bound, and validates with the same `replay` function as
   `SessionJournal::open`. There is one validator.
   - On Windows, a lock or sharing violation becomes `SessionError::Busy`.
     There is no retry.
   - Bytes after the last line feed are ignored and reported as
     `incomplete_tail`, because on Linux an observer can read while a writer is
     appending. The file is never changed. `SessionJournal::open` still rejects
     such a file.
   - The result adds `created_at`, `updated_at`, and an ordered list of
     transcript entries. A request manifest entry carries the model, prompt
     version, and tool names only.
2. **Bounded discovery.** `session::discover` walks only
   `sessions/<YYYY>/<MM>/<DD>/` and lists files that match the writer's exact
   file name pattern. It never follows a symbolic link or Windows reparse
   point, never opens a journal, and stops with `LimitReached` after 50,000
   journals or 500,000 directory entries. Skipped links and unrecognized
   entries are counted.
3. **A derived SQLite index.** `session_index::SessionIndex` stores one row
   per journal and an FTS5 table of searchable text in
   `<data_root>/session-index-v1/index.sqlite3`, through `rusqlite` 0.43.0 with
   the bundled SQLite and no other feature.
   - The index is a cache. The journals stay the source of truth. A missing,
     corrupt, or foreign database is deleted and rebuilt; only the index's own
     two files are ever deleted.
   - Refresh is incremental and bounded. An unchanged `ok` row (same size,
     modification time, and path) is skipped without opening the journal. A
     call inspects at most 1,000 journals, one at a time, and commits one
     transaction per journal.
   - A `Busy` journal keeps its previous metadata and text and is retried on
     the next refresh. An invalid journal is listed as `invalid` with a code
     and has no searchable text. One bad journal never fails a refresh.
   - Indexed text: the title, user, assistant, and tool messages, and
     compaction summaries. Tool arguments, manifests, and instructions are not
     indexed.
4. **Search text is never query syntax.** Each whitespace-separated term is
   quoted, the terms are joined as a conjunction, the last term is a prefix
   match, and the expression is bound as a parameter. Snippet boundaries use
   two private-use code points that are removed from text before indexing, so
   transcript content cannot forge a highlighted match.
5. **Sessions are addressed by UUID.** `journal_path` resolves a UUID to a
   path for a later writer open. Because the database is a cache that could be
   altered, it revalidates the stored relative path against the strict layout
   and requires the file name's UUID to match. The writer open then validates
   the journal again by replay.
6. **Connection hardening.** The database is opened with `NOFOLLOW`, refuses a
   linked index directory or file, and sets `trusted_schema=OFF`,
   `secure_delete=ON`, defensive mode, and a 2-second busy timeout. SQL
   extension loading stays disabled. All statements are parameterized, and an
   ordering is chosen from a closed set of static clauses.

## Alternatives considered

- **Take a shared lock to read.** Rejected: even a brief shared lock makes a
  writer's non-blocking exclusive lock fail, so a background scan could make
  `omgcode chat` report `Busy`.
- **Scan every journal on each tree request.** Rejected: each journal can be
  8 MiB, and the contract forbids loading every transcript.
- **An in-memory index only.** Rejected once full-text search was selected: a
  cold start would re-read every journal.
- **A pure-Rust search engine such as tantivy.** Rejected: a much larger
  dependency tree and its own on-disk format, for a store that also needs
  ordinary metadata queries.
- **A hand-written inverted index.** Rejected: tokenization, ranking, and
  crash behaviour are solved problems in SQLite FTS5.
- **System SQLite instead of the bundled copy.** Rejected: FTS5 availability
  and version would vary by host. The bundled copy costs a C compile.
- **Truncating a search with more than 16 terms.** Rejected: dropping terms
  silently widens the result. Such a search is refused.

## Consequences

- The tree and search work while another process is writing, with a visible
  `busy` state on Windows instead of a failed open.
- The index file holds transcript text. It is protected like the journals:
  the directory is created private on Unix and inherits the per-user ACL on
  Windows. It is not encrypted.
- The build now compiles SQLite's C source through the `cc` crate, so a C
  compiler is required on every build host. `rusqlite` uses `unsafe`
  internally; the core crate still forbids it and uses only the safe API.
- Each indexed journal costs one synchronous commit, so a first build of many
  sessions can take seconds.
- Known limits, not yet handled:
  - Corruption inside a table behind a valid header is found at query time,
    not at open, and nothing rebuilds the index automatically.
  - A build with a different index version deletes and rebuilds the database,
    so two different versions sharing one data root rebuild each other's index.
  - Rebuild fails on Windows while another process holds the database open.
  - `journal_path` does not check that the file still exists.
  - A just-created journal can be observed empty: `invalid` on Unix, `busy` on
    Windows, until its first record is written.
- The Windows host that built this cannot create file symbolic links, so the
  two file-symlink refusal tests skip there. They, and the Unix-only branches
  (`O_NOFOLLOW`, inspection beside a live writer), ran on Linux under WSL 6.6.

## References

- [Session storage](../session-storage.md)
- [ADR-0007](0007-store-session-journals-as-dated-jsonl.md)
- [ADR-0016](0016-add-a-session-owned-tool-runtime-with-schema-v2.md)
- [ADR-0041](0041-compact-active-context-with-readable-summaries.md)
- [SQLite FTS5](https://www.sqlite.org/fts5.html)
- [SQLite defensive configuration](https://www.sqlite.org/c3ref/c_dbconfig_defensive.html)
