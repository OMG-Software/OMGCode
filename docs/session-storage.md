# Session storage and archiving

This document separates the current JSONL session prototype from the complete
release contract. Conversation and session refer to the same saved coding
interaction. The prototype stores a bounded transcript and archive state. A
separate journal records metadata for bounded file-broker operations. The
prototype provides lock-free inspection, discovery, and a derived SQLite
index that the desktop shell uses for a session tree and search (see
[ADR-0044](adr/0044-observe-journals-without-the-writer-lock-and-index-them-in-sqlite.md)
and the [desktop interface guide](desktop-interface.md)). It does not provide
task execution, a recovery interface, or storage-use display.

The [accepted replacement contract](file-access.md#replacement-contract)
uses a separate versioned ledger and private checkpoint store. Phase A2b adds
protected Linux storage and its writer. Phase A2c records stage, commit intent,
and outcome for trusted Linux apply in that same replacement ledger.
The existing strict file-operation-v1,
control-v1, and session schemas are unchanged. Checkpoint contents are never
stored in conversation events or returned by ordinary checkpoint inspection.
Session replay, archive, and restore cannot authorize or replay replacement.
From 0.49.0 new checkpoints live in each workspace's `.OMGCode` folder, and
the control journal records `workspace_store_bound` and
`workspace_store_forgotten` events; session storage is unchanged
([ADR-0070](adr/0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)).
Unregistering a workspace (`workspace_unregistered`,
[ADR-0081](adr/0081-unregister-a-workspace.md)) leaves its sessions' journals,
index entries, and recorded `workspace_id` unchanged. The identifier then
matches no registration, so a later turn in such a session is text only.

## Implemented prototype (v0.4.0)

The Rust core creates and opens one UTF-8 JSONL journal per session. The CLI
uses the operating system's per-user local application data location by default;
`--data-dir` is an explicit override. The default is outside the workspace, but
configured development roots reject overlap with application storage. Without a
configured root, legacy text-only sessions retain the previous unchecked
`--data-dir` override. No tool access is granted by that compatibility mode.
The journal path stays fixed
after creation. The filename contains the UTC creation time and session UUIDv4.
Creation uses exclusive file creation.

The journal holds an exclusive file lock for the lifetime of its writer. A
second writer receives a conflict. The prototype syncs every complete appended
event with `sync_all` before it updates the in-memory snapshot or reports the
append as successful. A write or sync error poisons that writer; it accepts no
further mutations. A complete event may still have reached storage when sync
reports an error, so the result can be uncertain. Reopen validates the journal
and does not retry recorded work.

Unix creation requests mode 0700 for new session directories and 0600 for a new
journal file. Existing directory permissions are not changed. Windows uses the
inherited directory ACL; explicit per-user ACL setup is not implemented. The
platform helper inspects an open journal handle for a regular, non-reparse,
single-link file. This does not contain parent-directory races or establish
workspace access confinement. Directory crash durability and Windows ACL
behavior have not been qualified. The implementation does not claim equivalent
crash durability on Windows and Unix.

Windows creation and reopening omit delete sharing, so a journal cannot be
renamed or replaced while its writer holds the file handle. This is separate
from the exclusive writer lock and the existing link checks.

The current prototype writes schema version 3. Every event also has the common
envelope described below, and payload objects reject unknown fields. A journal
must use one schema version throughout; mixed versions are rejected. New
sessions use version 3, so older binaries cannot open them. Version 1 and 2
journals remain readable and writable with their original subsets.

The version-1 subset is:

| Event type | Payload in v1 | State rule |
| --- | --- | --- |
| `session.started` | `title`, `workspace`, `workspace_id`, `app_version` | First event, sequence 0. `workspace_id` is the stable registry ID for new sessions with configured workspace settings; legacy/unconfigured sessions use a per-session association ID. `app_version` is the package version. |
| `message.recorded` | `role`, `source`, `text` | `user` must pair with `user_cli`; `assistant` must pair with `provider`. The source label is metadata and grants no authority. |
| `turn.started` | `request_id` | Requires an active, unarchived session with no active turn. |
| `request.manifest` | `request_id`, `manifest` | Must match the active turn. The manifest has `base_prompt`, `prompt_version`, `prompt_sha256`, `custom_instructions`, `model`, `base_url`, `context_tokens`, and `max_output_tokens`. It is historical metadata, not policy or permission. |
| `turn.completed` | `request_id` | Must match the active turn. |
| `turn.failed` | `request_id`, `code` | Must match the active turn. The code is nonempty, at most 128 UTF-8 bytes, and has no control characters. |
| `session.archived` | Empty object `{}` | Requires no active turn. Repeating the API operation is idempotent. |
| `session.restored` | Empty object `{}` | Repeating the API operation is idempotent. Restore starts no work. |

Version 2 keeps every version-1 event and adds tool events plus optional
manifest metadata. A version-1 journal cannot contain any of these additions.

| Event type | Payload in v2 | State rule |
| --- | --- | --- |
| `tool.requested` | `request_id`, `call_id`, `tool`, `arguments` | Requires an active turn. The call identifier is at most 128 bytes and unique within the turn. `arguments` is a strict JSON object of at most 32 KiB; the runtime accepts that much only for the create, edit, and multi-edit tools and 4 KiB for every other tool. It is untrusted model input, not a grant or execution result. A call the runtime rejected (ADR-0099) records `{}`. |
| `tool.completed` | `request_id`, `call_id`, `outcome`, optional `code`, `output_bytes`, `output_sha256` | Requires a matching pending request. `outcome` is `succeeded`, `failed`, or `denied`; failed or denied results require a bounded code. The digest and size cover the exact source-labelled tool message. |
| `message.recorded` role `tool` | `role`, `source` (`core_tool`), `text`, `call_id` | Requires a completed, unmessaged call. Text is the exact source-labelled untrusted content shown to the model, at most 16 KiB. A completed turn requires one tool message per completed call; a failed turn may leave an intent without a result. |

**Rejected calls (ADR-0099).** A call the runtime answers without running
(`unsupported_tool`, `arguments_too_large`, `invalid_arguments`, or
`invalid_path`) is still one `tool.requested`, one `tool.completed` with
outcome `failed` and that code, and one `tool` message, in that order, so
every call stays paired with its result and replay is unchanged. The request
records `{}` as its arguments, never the received text. A call to a tool the
runtime does not have records its name only when the name is at most 64 bytes of
letters, digits, `_`, `.`, and `-`, and otherwise `unsupported_tool`. The tool
message holds the received argument text's byte length and SHA-256, and for
`arguments_too_large` the tool's limit; when the result limit cannot hold
them, it keeps only the tool name and code. No schema version changed. This
build records up to 32 KiB of arguments for the create, edit, and multi-edit
tools and 4 KiB for every other tool. A build before 0.63.0 refuses to open a
journal that holds a `tool.requested` event over 4 KiB of arguments or any
event over 64 KiB, as an invalid journal; it does not repair it. A journal
opens in both only when every call is within 4 KiB and every event within
64 KiB. An event can pass 64 KiB with no large call, for example a message of
11 to 13 KiB of characters such as U+0001, each stored as a six-byte `\u0001`
escape. A 16 KiB message of them would exceed this build's 80 KiB event bound
and cannot be recorded.

**Web fetch calls (ADR-0098).** A `web_fetch` call is recorded like any
other call: one `tool.requested`, one `tool.completed`, and one `tool`
message with source `core_tool`. The message text is labelled `source
web_fetched; untrusted web content from <host>` instead of workspace data. A
URL the fetch policy refuses completes as `failed` with code `url_refused`.
Once a session holds a `web_fetch` call completed as `succeeded`, the
runtime treats it as having fetched web content: it derives this when the
journal is opened, so the session's later model actions are marked
`after_web_content` in the model action journal across turns, restarts, and
compaction, which keeps these events. Saved fetch results replay under the
same live `model_tools` and `read` rule as other tool results. A request
manifest may list ten tool names, with tool schema `0.8.0`; a build before
0.64.0 refuses a journal whose manifest lists ten. Since 0.65.0 the schema is
`0.9.0`, whose command tool lists the recorded toolchain profiles
(ADR-0100); the manifest still records only tool names, and earlier versions
replay unchanged.

The version-2 `request.manifest` payload adds optional `tool_schema_version` and
`tools` fields. They are present together only when the request advertised
tools. Version-1 manifests omit both fields.

Version 3 keeps every version-2 event and adds readable context compaction. A
version-1 or version-2 journal cannot contain these additions.

Since 0.39.0 a version-3 `request.manifest` may also carry `skills`: one to
four entries of `skill_id`, `name`, and `skill_md_sha256` for the skills the
operator selected with `chat --skill` (ADR-0057). The field is written only
when a turn selected a skill, so other journals keep their bytes, and 0.38.0
and earlier refuse a journal that has it; the schema version did not change.
A version-1 or version-2 journal refuses the field, and chat refuses a skill
selection in such a session with `skill_session_version` before any request.
Skill messages are never recorded, so replay and compaction never see them.
A recorded assistant reply can still repeat skill text.

Since 0.66.0 a version-3 `request.manifest` may also carry `reasoning_effort`:
one of `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`, the
effort the chat client sent with the turn's requests (ADR-0102). It is written
only when an effort was sent, so other journals keep their bytes, and 0.65.0
and earlier refuse a journal that has it; the schema version did not change.
A version-1 or version-2 journal refuses the field, and chat refuses an effort
in such a session with `session` before anything is recorded. Compaction
summary requests send no effort.

| Event type | Payload in v3 | State rule |
| --- | --- | --- |
| `context.compacted` | `summary`, `compacted_messages`, `includes_tool_results`, `summary_sha256`, `model`, optional `prompt_tokens`, optional `completion_tokens` | Requires an idle, unarchived session. The summary is at most 16 KiB minus a fixed untrusted-source label, and the digest is the lowercase SHA-256 of the summary. The boundary is within the message history, later than any prior compaction, and followed by a user message when any message is retained. The original messages remain. |

The snapshot reports the active compaction record when present. The synthetic
summary is a labelled user-level message; it is not a recorded journal message
and never becomes an instruction. A summary whose `includes_tool_results` is
true carries earlier workspace data: it is replayed only under live
`model_tools` and `read`, and is otherwise replaced by a withheld notice, like
a saved tool result. Chat and the turn runtime build active
history from the summary plus messages after the boundary. See
[ADR-0041](adr/0041-compact-active-context-with-readable-summaries.md).

`workspace` is a tagged native path: `{"encoding":"unix_bytes","units":[...]}`
stores Unix path bytes; `{"encoding":"windows_utf16","units":[...]}` stores
Windows UTF-16 code units. It is descriptive metadata only. It is not a current
workspace authorization. The registry supplies stable IDs for configured new
sessions; the journal cannot restore or mint authority. Existing journals are
not rewritten. See [workspace settings](workspace-permissions.md).

### Core turn/tool lifecycle

The `omgcode-core` `turn` module runs one bounded model/tool turn through a
caller-supplied model interface. `chat` runs it with the local LM Studio
adapter when the session's workspace enables `model_tools`; otherwise chat
advertises no tools and rejects tool calls. A caller must provide an open schema-version-2-or-later
journal, the current control-store writer, and the workspace ID from trusted
settings. The journal's recorded workspace ID is only a coherence check.

The runtime supports `workspace_read_file` and `workspace_list_directory`,
plus approval-gated tools offered only under their live settings:
`workspace_edit_file` (ADR-0043), `workspace_multi_edit_file` and
`workspace_create_file` (ADR-0054), `workspace_rename_file` and
`workspace_delete_file` (ADR-0055), `workspace_create_directory` (ADR-0079),
and `workspace_run_command` (ADR-0040). It parses strict OpenAI-style function
calls. A call with a usable, unique identifier whose tool is unknown, whose
arguments are too large or malformed, or whose path is absolute or has a parent
segment is answered with a recoverable result and the turn continues
(ADR-0099). A response element with no supported shape or identifier, and a
duplicate call identifier, end the turn before dispatch. For each operation it
opens the control store, obtains a fresh session binding, releases the settings
lock before the next model request, and never reconstructs authority from the
journal. The runtime allows eight calls per response and 1 to 256 calls per
turn, defaulting to 128. It reserves one model request per allowed call and
one for the final reply. Tool messages are bounded to 16 KiB each and 256 KiB
in aggregate.
The core defaults to a 30-second cooperative deadline. Chat defaults to 120
seconds and can use up to the core maximum of 3600 seconds. Approval waits
are excluded. A read call accepts at most
15 KiB of file content and defaults to 8 KiB; larger files are refused rather
than truncated. Before a tool-enabled turn starts, the history and pending
user message must leave room for two messages per configured tool call.
Before each dispatch the runtime reserves the largest possible
tool message against the assembled-prompt budget; if the next request could not
fit, it fails the turn before recording an intent or opening the target. Every
rendered tool message, including an error, must fit the selected per-message
limit and the 16 KiB session message limit before it is recorded. Interim
assistant text returned with tool calls is recorded and retained in the
follow-up history. Saved tool messages from earlier turns are sent again only
while the live `model_tools` and `read` preferences both allow it, checked when
the turn starts; otherwise each is replaced by a fixed withheld notice, and the
journal keeps the original. Text-only chat always withholds them. Call
identifiers are scoped to one turn: a failed turn may
leave a pending intent, and a later turn may reuse that identifier.
Cancellation and the deadline are checked between bounded operations; they do
not preempt a broker call already in progress.

`workspace_list_directory` returns a Unicode name as a JSON string. A native
name that cannot be represented as Unicode is returned with `name: null`,
`addressable: false`, and its lossless native units, so the model cannot mistake
it for an addressable path.

Every tool result is recorded first as `tool.requested`, then
`tool.completed`, then a `message.recorded` entry with role `tool` and source
`core_tool`. The tool text starts with a source label and remains user-level,
untrusted content when the next request is assembled. Model-requested text
cannot become a system message. Read and list run directly. An edit, a
multi-edit, a create, a rename, a delete, or a command runs only after a
matching single-use record that the operator approved; there are no general
file write tools, and Git, MCP, and network access remain unavailable. The separate
[file-operation journal](file-access.md) remains the broker's operation ledger.

The runtime does not redact by itself. The caller must redact user text and
custom instructions before the call, and a provider integration must redact
model text before recording. A caller may supply a tool-text filter. It runs on
each tool result before the size limit, digest, journal record, and next model
request. Tool-enabled chat uses it to remove the configured LM Studio token.
There is no other secret detection: workspace file content and native path
names are otherwise recorded as read. Treat the schema-version-2 journal and
its tool messages as private user data.

The CLI supports `session new`, `inspect`, `archive`, `restore`, and
`interrupt JOURNAL`. `inspect` also lists every recorded `tool.requested`
event as `tool_requests`, with its turn, call ID, tool name, and exact
arguments string, so the operator can review an edit's `old_text` and
`new_text` before approving it. The arguments are untrusted model data, and
listing them dispatches nothing. Interrupt marks a recorded active turn failed with the code
`operator_interrupted`; it does not retry the model request. The current Archive
operation refuses an active turn. The operator can interrupt it first, then
archive it. There are no session-owned processes or MCP connections to clean up
in this prototype. `chat --journal` does not resume an active turn; it requires
an explicit interrupt first.

Ordinary journals are limited to 8 MiB and 10,000 events. Each event is at most
80 KiB, a message or tool message is at most 16 KiB, tool arguments are at most
32 KiB, and a title is 1 to 256 UTF-8 bytes. Both decoded and encoded limits
apply. Active turns reserve one terminal event and
1 KiB for it. CLI chat preflights the encoded user message and manifest, plus
capacity for the reply and termination, before recording the turn marker.
The marker precedes the user message. Deterministic reply-size failures close
the turn; uncertain I/O failures poison the writer and retain recovery state.

A legacy journal already full with an active turn may append one terminal
recovery event of at most 1 KiB beyond ordinary quotas. The reader accepts at
most 8 MiB plus 1 KiB and 10,001 events, and permits overflow only for that
single matching terminal record. Ordinary records cannot use recovery capacity.
A full journal remains inspectable; it needs a new session for further chat
or other ordinary mutations. No event is discarded or rewritten to make room.
See [ADR-0011](adr/0011-reserve-terminal-journal-capacity.md).

The reader requires a final newline, accepts
LF and CRLF, and rejects a BOM, duplicate JSON keys, unknown or mixed schema
versions, unknown events, unknown fields, repeated event IDs, invalid
transitions, sequence gaps, a session ID that conflicts with the filename,
version-1 tool events, orphan or duplicate tool calls, and a completed turn
with pending or unmessaged tool calls. It validates
the complete journal before returning a writable handle. A malformed journal
or incomplete tail is rejected without mutation by the writer open. There is no
recovery interface. Lock-free inspection (below) reads such a journal without
modifying it; the desktop shell shows the transcript and an incomplete-tail
notice, and offers no repair.

The request manifest does not include usage, `finish_reason`, or tool-round
counts. The CLI returns provider usage and finish reason in its JSON output, but
does not append them to the journal. Schema version 2 records the exact tool
schema version and tool names advertised by the core turn runtime. The prototype
has no streaming, artifact, index, compaction, migration, or memory events.
Streamed model text (ADR-0049) is provisional and is never written to the
journal: the recorded reply is the validated whole reply, redacted as a whole,
and the journal does not say whether a reply was streamed. A turn cancelled
during a model request (ADR-0050, ADR-0051), streamed or not, records a
`turn.failed` event with the code `cancelled` and no final assistant reply;
its partial text is not recorded, and no record changes shape. An assistant
message recorded with the tool calls of an earlier round of the same turn
stays. A turn cancelled before its `turn.started` event records no turn
event, and a cancelled compaction writes no `context.compacted` event. A
compaction that finished before the cancellation keeps its
`context.compacted` event. The CLI redacts the currently configured LM Studio token from
new conversation text, preferences, titles, and replies before retention and
output. It redacts that same token from historical context before inference.
There is no general secret detector or automatic cleanup of older stored text.
The token itself is never intentionally added to the request manifest.

CLI results carry a lossless native journal locator as well as separate display
text. The journal string is null for paths that are not Unicode. Pass the
journal_native JSON to --journal-native when reopening on the same OS.
Post-open chat errors include this locator under error.session.

These are implemented prototype facts, not a claim that the full release
contract below is complete.

### Lock-free inspection and discovery (v0.29.0)

`SessionInspection::inspect` reads one journal without calling any lock
function, so a viewer never makes a writer report `Busy`. It opens the file
read-only without following links (`O_NOFOLLOW` on Unix;
`FILE_FLAG_OPEN_REPARSE_POINT` with read and write sharing on Windows), runs the
platform handle validation, reads within the 8 MiB journal bound plus 1 KiB, and
validates with the same `replay` function as `SessionJournal::open`.

- **Windows.** The writer lock is mandatory. A lock violation (error 33) or a
  sharing violation (error 32) becomes `SessionError::Busy`, with no retry.
- **Unix.** The writer lock is advisory, so inspection beside a live writer
  succeeds. A Linux test run on WSL 6.6 covers that branch.
- **Incomplete tail.** Bytes after the last line feed are ignored and reported
  as `incomplete_tail`. `event_count` counts complete events only. The file is
  never modified. A file with no line feed at all is `InvalidJournal`.
  `SessionJournal::open` still rejects an incomplete tail.
- **Result.** The session and workspace identifiers, the recorded workspace
  path, title, archive state, active turn, schema version, event count,
  `created_at` (from `session.started`), `updated_at` (the last complete
  event), the tail flag, and an ordered entry list. Entry kinds are message,
  turn started, turn completed, turn failed, tool requested, tool completed,
  request manifest, context compacted, session archived, and session restored. A
  manifest entry carries the model, prompt version, and tool names only.

`session::discover` walks only
`sessions/<4 digits>/<2 digits>/<2 digits>/<file>` under the data root. A file
name must match the writer's exact pattern, with a strict parser for the
lowercase hyphenated UUIDv4 and the time fields. It never follows a symbolic
link or a Windows reparse point, never opens a journal, counts skipped links and
unrecognized entries, and stops with `LimitReached` after 50,000 journals or
500,000 directory entries. A missing `sessions` directory is an empty result.
`session::locate` resolves a UUID by a full walk; two matches are an error.
`SessionJournal::open` and `inspect` keep the older lenient file-name UUID
parse, so a journal with an uppercase UUID opens by path but is never listed.

### Session index

`session_index::SessionIndex` is the derived search and tree index. It lives in
`<data_root>/session-index-v1/index.sqlite3`. The journals remain the source of
truth, and the index authorizes nothing.

- **Storage.** `rusqlite` 0.43.0 with bundled SQLite 3.54.1. A rollback journal
  (no WAL), `foreign_keys=ON`, `trusted_schema=OFF`, `secure_delete=ON`,
  defensive mode, SQL extension loading disabled, and a 2 second busy timeout.
  `application_id` is `0x4F4D4749` and `user_version` is 1. The file is opened
  without following links. An index directory or file that is a link or
  reparse point is refused. That check is a `symlink_metadata` call before the
  open; a race between the check and the open is not excluded.
- **Rebuild.** The index deletes and recreates only its own two files
  (`index.sqlite3` and `index.sqlite3-journal`) when the file is not SQLite or
  is corrupt at open, when `application_id` differs, when `user_version` is not
  1, or when the header is valid but the tables are missing.
- **Content.** One `journal` row per session (path, size, modification time,
  state `ok`, `busy`, or `invalid`, title, workspace, times, archive state,
  active turn, event count, schema version, tail flag) and an FTS5 table of
  searchable text with the `unicode61 remove_diacritics 2` tokenizer. Indexed
  text is the title, user, assistant, and tool messages, and compaction
  summaries. Tool-call arguments, request manifests, custom instructions,
  outcomes, and failure codes are not indexed. The two private-use code points
  used as snippet markers are replaced before indexing.
- **Refresh.** An unchanged `ok` row (same size, modification time, and path) is
  skipped without opening the journal. A call inspects at most 1,000 journals,
  one at a time, with one transaction each, and removes rows for vanished
  journals. A `Busy` journal keeps its previous metadata and text and is
  retried. A journal that fails validation becomes `invalid` with a code and no
  text, and is not re-inspected until the file changes. `io_error` and
  `duplicate_session_id` are always re-evaluated. One bad journal never fails a
  refresh.
- **Queries.** `sessions` lists an active or archived view sorted by update
  time, creation time, or title, with a limit of 1 to 5,000. `search` takes at
  most 256 characters and 16 terms, quotes every term, joins them with AND, and
  treats the last as a prefix. A longer query is refused, not truncated. The
  limit is 1 to 200, and hits are ordered by relevance, then update time, then
  session identifier, then sequence number. Snippets come back as marked parts.
- **Path resolution.** `journal_path` revalidates the stored relative path
  against the strict layout and the file name's UUID before it returns a path.
  It does not check that the file exists. The writer open validates the journal
  again by replay.

Known limits:

- Corruption inside a table behind a valid header is found at query time, and
  nothing rebuilds the index automatically.
- A binary with a different index version deletes and rebuilds the database, so
  two versions that share a data root rebuild each other's index.
- Rebuild fails on Windows while another process holds the database open.
- Each indexed journal costs one synchronous commit, so a first build of many
  sessions can take seconds.
- A timestamp outside 1677 to 2262 marks a journal invalid.
- A just-created journal can be observed empty: `invalid` on Unix and `busy` on
  Windows, until its first record is written.
- The index holds transcript text. It is protected like the journals and is not
  encrypted.

What the desktop shell does with journals. It lists, searches, and pages
transcripts through the index and inspection. For archive, restore, and
interrupt it opens the writer, appends one event, and releases the journal, so
those operations take the ordinary writer lock. The shell refuses archive while
a turn runs in the same process and while a turn is open. It writes no
`session.archive_requested` event; Stop and archive is not implemented. See the
[desktop interface guide](desktop-interface.md#session-tree).

## File-operation journal

A read-only `file diff` preview performs the normal broker read and therefore
adds a normal read intent/outcome pair to the file-operation journal. Its
trusted proposed content is not recorded there. See the
[file-access contract](file-access.md) for preview bounds and no-mutation rules.

The workspace file broker records operation intent and outcome in a separate
`file-operations-v1.jsonl` file under the private application data directory.
It does not change the session JSONL event schema or the `control-v1.jsonl`
settings journal. Existing text-only sessions remain readable. A session journal's
workspace path or ID never creates a current binding or grants file access.

The file-operation log stores operation metadata, including native relative
paths, but never file contents. Treat path metadata as private user data. A
read result is returned only after its outcome is synced. A create that may have
changed the filesystem but lacks a durable outcome remains unresolved. The
operator must inspect the file and audit record, then explicitly acknowledge
inspection. Acknowledgement does not retry, repair, or remove the file. See the
[workspace file-access contract](file-access.md) for native operations and
recovery limits.

## Location and file naming

Save each session in its own UTF-8 JSONL file under OMGCode's per-user
application data directory, outside the development workspace:

```text
sessions/<year>/<month>/<date>/omgcode-<time>-<guid>.jsonl
sessions/2026/11/08/omgcode-12-34-56-789Z-7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470.jsonl
```

Use the session's creation time in UTC. Year has four digits; month and date
have two digits. Date means the day of the month. Time uses HH-MM-SS-mmmZ, with
milliseconds and no colon, so the filename is valid on Windows and Linux. GUID
means a randomly generated UUIDv4 in lowercase hyphenated form. Create the file
exclusively; a collision must never overwrite an existing session.

For release, choose the normal per-user local application data location through
platform APIs. Keep the storage root in trusted application settings. Do not
derive it from a repository instruction or model-supplied path. Storage
relocation needs a separate controlled migration with writers stopped and
recovery defined. The prototype uses this default in the CLI but also exposes
`--data-dir` for an explicit caller-selected override.

Keep the original path when a session crosses midnight, resumes on another day,
changes title/workspace, or is archived. The date tree organizes files; the UI
tree groups sessions by their recorded workspace identity and folder. Store
lossless native paths where required; display text is never an authorization key.

This is OMGCode's own versioned schema with the requested date-based layout.
It does not promise compatibility with ChatGPT exports or Codex internal event
schemas. Do not read or modify another application's session directory. Any
future importer needs its own explicit format and trust contract.

## Release JSONL event contract

Write one JSON object per line, without a byte-order mark or pretty-printed
multiline records. Escape newlines inside strings. Writers end every complete
record with LF; readers accept LF or CRLF. Each event has this envelope:

| Field | Contract |
| --- | --- |
| schema_version | Integer schema version; initially 1 |
| session_id | The UUID in the filename; constant for the journal |
| event_id | Unique UUID for this event |
| seq | Contiguous integer sequence starting at zero; authoritative order |
| timestamp | RFC 3339 UTC event time; clock changes must not reorder events |
| type | Defined event type |
| payload | Object validated against that event type's schema |

The release schema must define the first `session.started` event and every
supported event's payload, size limits, state transitions, and migration rules
before release. The prototype supports only the v1 event subset listed above.
Release requirements include title and workspace changes, bounded streaming
updates, tool intents and results, compaction records, usage, errors, and
archive/restore events. Preserve source labels and causal request/tool IDs.
Distinguish model-requested actions from core-observed execution results.

The following is an illustrative valid Unix v1 subset. Its UUIDs and timestamp
are examples. `workspace.units` contains the bytes for `/work/project`. The
example is not evidence of release schema compatibility:

```jsonl
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"f23d5e17-ef07-40dd-8db2-6e6b4a0ce12b","seq":0,"timestamp":"2026-11-08T12:34:56.789Z","type":"session.started","payload":{"title":"Project overview","workspace":{"encoding":"unix_bytes","units":[47,119,111,114,107,47,112,114,111,106,101,99,116]},"workspace_id":"8a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","app_version":"0.1.0"}}
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"eac73dca-838e-4802-a0eb-8bafbf00cd7c","seq":1,"timestamp":"2026-11-08T12:34:57.000Z","type":"message.recorded","payload":{"role":"user","source":"user_cli","text":"Explain this project."}}
```

For release, define and version the complete event schemas, limits, and
migration rules. Preserve unsupported data for inspection, but do not resume a
journal with unknown required semantics or schema versions. Invalid sequence
numbers, duplicate IDs/keys, or conflicting session IDs require a visible
recovery/read-only state, not silent reinterpretation. The current prototype
rejects such journals and leaves their bytes unchanged; it has no recovery
artifact or repair. Lock-free inspection gives a read-only view of a journal
that passes validation, with an incomplete tail reported. A journal that fails
validation is listed as `invalid` with a code and no transcript. Unsupported
records remain in the original file.

Store large outputs and binary attachments as bounded artifacts referenced by
safe session-local identifiers, with content digests and availability status.
Validate references within application-managed storage; never treat a transcript
path or URI as permission to read a host file or contact a service. Missing
artifacts remain visible as missing. Do not invent their content.

## Release durability, recovery, and locking

The JSONL journal is the source of truth for transcript and archive state. A
search/tree index is derived and rebuildable. The prototype's index is the
[SQLite index](#session-index) above. Losing or clearing a cache/index must not
delete the journal. Use incremental indexing and bounded streaming reads; do not
load every transcript into memory.

Release requirements not yet met by the prototype: the writer is the existing
exclusive file lock, not a single Rust-owned owner with a bounded queue that
other windows route through; there is no recovery artifact for an incomplete
tail; and there is no automatic repair of a corrupt index.

Use one Rust-owned writer per session, with a bounded queue and exclusive
cross-process ownership. Multiple windows route through that owner or receive a
clear conflict. Repeated event submissions must not append duplicate IDs.
Flush buffered stream chunks at defined boundaries. Synchronize critical events
to durable storage before acknowledging session creation, archive, or restore.
Persist the intent/identity before dispatching an operation with side effects.
Specify file and directory durability guarantees for each supported OS/filesystem.

On restart, read complete valid records up to the last newline. Preserve an
incomplete tail in a recovery artifact before repairing it under exclusive
ownership. Report the recovery and record it; never silently discard evidence.
Interior corruption requires recovery/read-only handling instead of skipping
records and resuming. File replacement/migration must keep a recoverable original.

If persistence fails, report the failure and stop dispatching new mutations that
cannot be recorded. If an external action may already have occurred, retain its
uncertain status and reconcile state before retry. A persisted intent without a
result is incomplete, not permission to replay. JSONL alone cannot provide
exactly-once execution of external side effects.

## Release archive, restore, and resume

The desktop prototype provides Archive, Restore, and an Archived view in the
tree with search and transcript inspection. It does not provide Stop and
archive, a `session.archive_requested` event, or storage-use display.

Provide Archive in the session tree and conversation view. Hide archived sessions
from the active tree, while keeping an Archived view/filter with folder grouping,
search, transcript inspection, and Restore. Archive preserves the same journal,
session ID, attachments, title, and workspace association. It does not move or
delete the JSONL file, compact history, or erase provider-retained data.

For an idle session, release session-owned execution resources and MCP
connections, append session.archived durably, then update the index/UI. Repeated
archive requests are idempotent. Do not report archive complete if local cleanup
failed or remains unobserved.

For active turns/background work, offer Archive when idle or Stop and archive.
Persist a session.archive_requested event with its request ID and selected mode.
Show the pending state and allow cancellation through a recorded state change.
After restart, recover that intent without starting a new turn or replaying work.
Archive when idle blocks new turns and unrelated background tasks but permits
bounded work needed to finish the existing turn. Stop and archive blocks further
tool dispatch and cancels/observes affected local tasks immediately.

Both modes release session-scoped MCP connections and execution resources before
the archive event is committed. Do not affect other sessions. Unknown remote
outcomes remain explicitly recorded; do not claim that cancellation rolled back
remote effects.

Restore appends session.restored durably and returns the session to its workspace
group. Restore is also idempotent and starts no execution. Resuming requires a
separate user action. Revalidate the current workspace/root, provider/account,
permissions, enabled skills/MCP servers, and instruction revisions. A missing,
moved, or disallowed workspace needs explicit resolution. Old grants, paths,
tool calls, or connection records must never restart work by transcript replay.

Archived sessions remain retained by default. Show their storage use separately
from caches. Permanent deletion, export, and retention cleanup are distinct
actions with explicit scope; archiving and cache clearing cannot imply deletion.

User-saved [memories](memories.md) have their own store and lifecycle. Archive or
cache cleanup does not delete them. Record selected memory IDs/revisions with
request manifests, but never recreate deleted memories or restore old scope
choices by replaying a journal. Historical snapshots follow journal retention;
memory deletion does not promise removal of every historical copy.

## Release privacy and trust

Keep journals, indexes, recovery artifacts, and attachments outside every task's
writable view. Deny indirect access through links and overlapping roots. Use
per-user file protections and safe core APIs; the frontend does not choose raw
journal paths. JSONL is readable text, not encryption or tamper evidence.

Never persist authentication tokens, passwords, authorization headers, login
codes, or secret-bearing environment/arguments as application diagnostics. Use
safe credential references and redact known sensitive fields before queuing an
event. User messages and tool output can still contain sensitive project data;
do not claim automatic detection of every secret. Protect backups and exports
under their own explicit policy.

Validate journals as untrusted input when reading. A role label, approval event,
or recorded policy snapshot cannot grant current authority. Use current
core-owned policy and operation records for authorization/reconciliation. Safely
render all message/tool content. Preserve the provenance and invalidation rules
from [agent instructions](agent-instructions.md) and
[context management](context-management.md).

## Release acceptance requirements

Test Windows/Linux naming, UTC midnight and timezone changes, clock rollback,
UUID collisions, Unicode/native paths, stream reconstruction, malformed/large
records, unknown schemas, duplicate IDs/keys, multi-writer contention, interrupted
append, disk full, interrupted index updates, index rebuild, missing artifacts,
link/path attacks, redaction, archive/restore idempotency, active-task shutdown,
MCP disconnection, crash recovery, changed workspace/permissions on resume, and
absence of tool/remote side-effect replay. Validate the documented JSONL examples
as examples only; they do not establish runtime persistence guarantees.

## Reference boundary

[OpenAI's developer command documentation](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
describes archive/restore without deleting a saved transcript. This is a useful
behavioural reference. The exact path, event schema, and durability requirements
above are OMGCode decisions, not a claimed OpenAI storage-format contract.
