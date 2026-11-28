# 0109: Drive the signed-in Claude Code CLI as a model connection

- Status: Accepted
- Date: 2026-11-28

## Context

[ADR-0002](0002-require-subscription-authentication.md) makes Claude
subscription use a release requirement and keeps its route visibly
unresolved. It says that a CLI wrapper does not establish third-party
subscription permission, and that OMGCode must not import another
application's credentials.

Anthropic's Claude Code legal and compliance page was read again on
2026-11-28. It bars third-party developers from offering Claude.ai sign-in in
their own applications. It bars them from routing requests through Free, Pro,
Max, or Ultra plan credentials on behalf of their users, and from collecting,
storing, or brokering Claude.ai credentials or session tokens. It does not
stop an end user from signing in to the unmodified Claude Code binary with
their own subscription, and it says plan limits assume ordinary, individual
use of Claude Code and the Agent SDK. The Agent SDK overview still requires
prior approval for a third party to offer claude.ai login or rate limits. No
page says whether a separate program may drive the signed-in `claude` CLI for
its own user. The authentication page says `claude setup-token` makes a
one-year OAuth token for `CLAUDE_CODE_OAUTH_TOKEN`, and that
`CLAUDE_CONFIG_DIR` moves `.credentials.json` into that folder.

The project owner chose to drive the unmodified CLI that they sign in to
themselves, with OMGCode never handling the Claude credential. Tool calls
come back over a relay and a local pipe. The first change covers core and the
trusted CLI.

### Observed CLI behaviour

These probes used Claude Code 2.1.325 on Windows 11 on 2026-11-28, with an
environment built from scratch. Three of them used a signed-out, empty
configuration folder. One used the owner's existing sign-in for a single
Haiku request with one tool call.

- With `--tools ""`, the `system/init` event listed only the MCP tool. With
  `--disable-slash-commands`, it also listed no skills or slash commands.
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and `DISABLE_TELEMETRY=1`
  turned analytics off and removed the telemetry plugin.
  `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` removed the auto-memory folder.
  `--settings` with `enabledPlugins` set to `false` removed the three
  built-in plugins (`cc-plugin-agents-md`, `cc-plugin-privacy`, and
  `cc-plugin-plugin-authoring`).
- Canary `CLAUDE.md` and `AGENTS.md` files in the working folder did not
  reach the model under `--setting-sources ""` with those plugins disabled.
  The model answered that no canary text was in its context.
- The MCP client first sent the 2026-07-28 `server/discover` request. After a
  `-32601` error it fell back to `initialize` with protocol version
  `2025-11-25`, then `tools/list`. Each `tools/call` carried
  `_meta["claudecode/toolUseId"]`, which equals the `tool_use.id` of the
  assistant event on stdout.
- A signed-out run printed `Not logged in` as assistant text, with
  `"error":"authentication_failed"`. Its `result` event had
  `"subtype":"success"` with `"is_error":true` and
  `"terminal_reason":"api_error"`. A signed-in run reported
  `"apiKeySource":"none"`.
- In stream-json input mode, `system/init` was written only after the first
  input line arrived, so its report cannot be checked before the first model
  request.
- `--bare` never reads OAuth, so it cannot use a subscription.
- The owner's own process environment held `ANTHROPIC_BASE_URL` and several
  `CLAUDE_CODE_*` variables from another host. An inherited environment would
  have redirected the child's traffic and changed its authentication.

## Decision

1. **A `claude_code` connection kind.** A provider profile of kind
   `claude_code` accepts authentication `subscription` only. It records the
   canonical path of the operator's `claude` program (an `.exe` on Windows)
   and the operator's Claude configuration folder, each with its recorded
   directory identity. It records no endpoint, account, token, or credential
   binding. The trusted CLI adds one with `workspace claude-code-add`; the
   generic `provider-profile-add` refuses the kind. The profile is
   configuration only: it does not show that Anthropic approves the route.
2. **The operator signs in, and OMGCode stays out of it.** The operator runs
   `claude auth login` with `CLAUDE_CONFIG_DIR` set to a folder used only for
   OMGCode, outside application storage, the development root, every
   workspace, every workspace container grant, every other connection's
   sign-in folder, and every other connection's program and relay grants.
   Recording the connection, granting it, changing the development root,
   adding a workspace container grant, and every turn check this, and a
   program or relay folder must not overlap another connection's sign-in
   folder either. A grant or sign-in record whose folder changed proves no
   separation: a moved folder can keep its access-control entries and lie
   anywhere, so the check refuses until that grant is revoked or that
   connection removed. OMGCode has no Claude sign-in command. It never opens, reads,
   copies, stores, or passes a file or token from that folder. It only checks
   the folder's identity, sets `CLAUDE_CONFIG_DIR` for the child, and, on
   Windows, grants the child's container access to the folder.
   `claude setup-token`, `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and
   `ANTHROPIC_AUTH_TOKEN` are never set, so the CLI uses only its own
   sign-in. A dedicated folder keeps this sign-in's refresh-token rotation
   apart from the operator's everyday Claude Code sign-in.
3. **A dedicated sandbox per connection.** On Windows, the child runs in its
   own AppContainer, `OMGCode.ClaudeCode.` followed by the profile
   identifier, with the `internetClient` capability. The operator grants it,
   with `workspace claude-code-grant`, read-execute on the program's folder,
   on the folder of the `omgcode` executable that runs the command (the
   relay), and on the connection's runtime folder, and read-write on the
   configuration folder. The runtime folder, `claude-code/<profile id>` in
   application storage, is the only part of storage the container can read,
   and it cannot write there. Each grant is recorded before and after its
   ACL change, as workspace grants are. A profile with grants cannot be
   removed; `workspace claude-code-revoke` revokes them all and deletes the
   container profile, also when no grant remains, so an interrupted deletion
   can be retried, and refuses a profile of another kind. A grant checks its
   whole plan before it records the first intent. Every turn checks that each
   role holds an active grant for the folder it names. A workspace container is never used: a model-run
   command there must not reach the sign-in folder, and the workspace
   container must not gain a network capability. On Linux, Landlock allows
   reading the program and relay folders, the per-turn folder, the system
   library and program folders, the network support files, and the random
   and zero devices, and writing the configuration folder, the per-turn
   temporary folder, and `/dev/null`. The seccomp filter allows IP sockets
   and `AF_UNIX` (`NetworkAccess::Internet` with
   `UnixSocketAccess::HostServices`). The run has the usual tree limits (4 GiB
   of memory, 512 processes, and four hours of CPU time), so on Linux the
   CLI must run in a delegated cgroup, as `run` does. Neither platform limits
   the child to Anthropic's hosts.

   A revocation or removal stops the connection's running turns, as ADR-0061
   does for runs. Each `claude` process holds a shared lock on the
   connection's turn lock file, `claude-code/<profile id>.turns`, which lies
   beside the runtime folder and outside every grant. Before the per-turn
   folder is made, the turn takes that lock and checks every settings state
   recorded since it resolved the connection. It releases the lock only after
   the tree was observed ended; when the end cannot be confirmed, the lock
   stays held until the OMGCode process exits, so a revocation reports the
   turn as running. While the process runs, a watcher thread checks each
   later state every 100 ms, and once more as it stops, so the turn's
   result counts only after every state up to its end was checked. A state
   that removes the connection or, on Windows, leaves one of its roles
   without an active grant ends the tree, and the turn fails with
   `claude_code_withdrawn`. Unreadable settings and a watcher that panicked
   withdraw the turn too; settings that another command holds are read on
   the next poll. `workspace claude-code-revoke`, `workspace
   provider-profile-remove`, and the `settings` window's removal record their
   change, release the settings, and then wait up to 30 seconds for an
   exclusive lock. When a turn does not end in time, the command fails with a
   `control` error that says the change is recorded. A revocation that fails
   after it recorded a withdrawal waits the same way and reports both its
   error and the wait's outcome. On Linux no grant exists, so
   `claude-code-revoke` changes nothing there and `provider-profile-remove`
   withdraws a connection.
4. **One process per model request, launched locked down.** A tool turn
   keeps one `claude` process for all its rounds; a text turn and each
   compaction summary start their own. Each run writes a fresh per-turn
   folder under the runtime folder that holds the system prompt, the MCP
   configuration, the settings, and an empty working folder. Its environment
   is built from nothing, with only these variables:
   - on Windows, `SystemRoot`, `windir`, `LOCALAPPDATA`, `PATH` set to
     `System32`, and `USERPROFILE` and `HOME` set to the container's profile
     folder; on Linux, `HOME` and `TMPDIR` set to the per-turn temporary
     folder, `PATH=/usr/bin:/bin`, and `LANG=C.UTF-8`;
   - `CLAUDE_CONFIG_DIR`;
   - `DISABLE_AUTOUPDATER=1`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`,
     `DISABLE_TELEMETRY=1`, and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`;
   - `CLAUDE_CODE_MAX_OUTPUT_TOKENS` from the reply budget;
   - `MCP_TIMEOUT=60000`, and `MCP_TOOL_TIMEOUT=3600000` so that an approval
     wait does not time the call out inside `claude`.

   The arguments are `-p`, `--output-format stream-json`, `--verbose`,
   `--include-partial-messages`, `--tools ""`, `--strict-mcp-config`,
   `--mcp-config <file>`, `--setting-sources ""`, `--settings <file>` (the
   four built-in plugins `cc-plugin-agents-md`, `cc-plugin-privacy`,
   `cc-plugin-plugin-authoring`, and `cc-plugin-telemetry` disabled),
   `--system-prompt-file <file>`, `--no-session-persistence`,
   `--disable-slash-commands`, `--permission-prompts none`,
   `--permission-mode dontAsk`, `--model <model>`,
   `--allowedTools mcp__omgcode__*` when tools are offered, and
   `--effort <effort>` when the session has one (`low` to `max`; `none` and
   `minimal` are refused as `effort_refused`). A run without tools gets an
   MCP configuration with no servers. The prompt goes to stdin as text, and
   stdin is then closed. The process deadline is one hour; each model
   request also waits at most the turn timeout (`chat`) or 600 seconds
   (`session compact`). A cancelled turn starts no process. The reader
   threads hand the turn at most eight queued events, so a turn that is
   running a call or waiting for an approval holds back the process's output.
   Every write to the relay ends at the request timeout or on cancellation:
   the channel is shut and the process tree ended, and the tree's end closes
   the relay's side, so the write ends even when the shutdown request fails.
   The writer and its watcher record the write's end with one
   compare-and-swap, so a write that finished first is never reported as
   stopped. Before an answer is written, the relay's events already queued
   are handled and the process's output is kept, in order, for the next
   round, so a cancellation sent while the runtime ran the call withholds
   the answer.
   The main agent's streamed and final text share an 8 MiB budget, and more
   fails the turn as too large. A `result` counts only after the whole prompt
   was written. After the tree ends, each worker thread is joined within five
   seconds.
5. **Lockdown is checked, and it fails closed.** The `system/init` event must
   satisfy all of these, or the tree is terminated and the turn fails with
   `claude_code_lockdown`:
   - exactly the offered tools, named `mcp__omgcode__<tool>`, or no tools and
     no MCP server for a run without tools;
   - otherwise exactly one MCP server, `omgcode`, with status `connected`;
   - no plugins, skills, or slash commands;
   - `apiKeySource` `none`.

   Because that event follows the first request, each tool block is checked
   too. A block that names anything other than an offered tool terminates the
   tree at once, from its first streamed `content_block_start`. No call is
   returned to the runtime before the init event passed.
6. **Tools come back through a relay over a local pipe.** The MCP
   configuration starts the hidden `omgcode mcp-relay <address>` subcommand
   as the only server. The relay runs in the same sandbox and copies bytes
   between its stdio and one local channel. It sends a per-turn nonce from
   its environment first, and the trusted process compares it in constant
   time. On Windows the channel is a named pipe that admits the user and the
   connection's container. Its mandatory label is Low with no-write-up, and
   the server accepts a client only after the client's token shows that
   container's SID. On Linux it is a socket in a per-turn `0700` folder
   with a random name, created exclusively under `XDG_RUNTIME_DIR`, or under
   `/tmp` when that variable is unset, relative, too long, or not UTF-8, since
   the relay's configuration is JSON. A socket path
   must fit in the 107 bytes of `sun_path`, and one beneath the runtime
   folder in application storage does not. All
   MCP handling stays in the trusted process. It answers `server/discover`
   with `-32601` so the observed client falls back to `initialize` at
   `2025-11-25`, offers only `tools`, and never sends a request to the
   client. A request identifier may be used once per session; the server
   records at most 4096 and then refuses further requests, and a stream
   line's `type` and `subtype` must be strings. A relay that does not connect
   within 60 seconds fails the run.
7. **The adapter is an ordinary `ToolModel`.** `ClaudeCodeModel` implements
   the core `ToolModel` trait. On a round it returns each pending
   `tools/call` as a tool call: the identifier is its `claudecode/toolUseId`
   (or, when absent, `omgcode_call_N` with an `N` that no other call of the
   round uses), the name is the MCP name, and the
   arguments are re-serialised. A round holds at most eight calls and
   returns once it is full, once every announced tool block has arrived, or
   200 ms after the last call. Calls are returned only at the end of a
   batch, which is every event already queued when it began, so a call whose
   `notifications/cancelled` is already queued is retired and never reaches
   the runtime, and a steady stream of events cannot hold a ready call
   back. The MCP request stays open while the turn
   runtime parses the call, applies the workspace policy, waits for any
   approval, dispatches, filters, and journals it, exactly as for every
   other provider. The next round answers each held request with the result
   text the runtime recorded, matched by call identifier. The `result` event
   ends the turn with its final text. No tool runs outside the runtime's
   dispatcher, and claude's own permission system decides nothing.
8. **OMGCode's journal is the only history.** Because each process lives for
   one turn with `--no-session-persistence`, OMGCode renders the assembled
   messages into its first prompt. System messages go to the system prompt
   file, followed by a note that names the `mcp__omgcode__` tools. Every
   other message is written to stdin as one transcript, oldest first, each
   under a label that carries a random per-turn marker. The labels are
   advice to the model, not a security boundary: the transcript is
   untrusted conversation data, as every provider's history is. The
   rendered prompt is at most 4 MiB, checked before any part is copied.
   Within a turn, claude keeps its own context.
9. **Failures are named.** A failure that the launch, sign-in, lockdown,
   protocol, child exit, or a withdrawal causes is reported as one of
   `claude_code_not_signed_in`, `claude_code_lockdown`,
   `claude_code_protocol`, `claude_code_failed`, or `claude_code_withdrawn`,
   in a chat turn and in compaction alike. Cancellation and the deadlines
   terminate the tree, and its end is observed before the turn returns. A
   failed termination or emptiness check counts as unconfirmed cleanup, so
   the tree's own fallback kill still runs. A streaming child reports itself
   ended once its tree was observed empty, also when the usage or disk
   accounting after that fails. Each request manifest records the endpoint label
   `claude-code:<profile>`, which is not a URL.
10. **The trusted CLI only, for now.** `chat --provider-profile` and
    `session compact --provider-profile` resolve a `claude_code` profile.
    Every other route that resolves a connection, including the desktop
    shell, the active selection, and `models`, refuses it with
    `claude_code_cli_only`.
11. **Terms and readiness.** The connection is personal use of the
    operator's own sign-in to the unmodified binary. OMGCode offers no
    Claude.ai sign-in and stores, reads, and brokers no Claude credential.
    It is not evidence of Anthropic approval. The release requirement for
    Claude subscription support stays open until Anthropic approves the
    route or documents a supported one.

## Alternatives considered

- **Store a `claude setup-token` token in protected storage and pass
  `CLAUDE_CODE_OAUTH_TOKEN`.** This is a smaller setup, but OMGCode would
  collect, store, and broker a Claude.ai session token, which the terms bar.
- **Copy the operator's `.credentials.json` into an isolated folder.** This
  reads another application's credential, which ADR-0002 rejects. The two
  copies would also race on refresh-token rotation.
- **Run `claude` in the workspace's AppContainer.** That is what the generic
  request described. It would give the workspace container a network
  capability, and any model-run program in that workspace could read the
  sign-in folder.
- **Route MCP through the Agent SDK control protocol** (`type: "sdk"`). This
  needs no relay or pipe, but its wire format is internal and undocumented,
  and any CLI update could change it.
- **Serve MCP over loopback HTTP.** AppContainer denies loopback without an
  exemption, and the owner ruled out network transports for tools.
- **Keep one `claude` process per session (`--input-format stream-json`), or
  resume with `--resume`.** This would reuse claude's context across turns,
  but claude would then keep state or a second transcript that OMGCode does
  not journal. An application restart would also lose it.
- **Use `--bare`.** It never reads OAuth, so it cannot use the subscription.
- **Keep the Linux socket under application storage and have the relay
  connect through `/proc/self/fd/<folder>/mcp.sock`.** The path would then
  fit `sun_path`, but the relay would depend on procfs inside the sandbox
  and on a descriptor trick that is harder to review. A short private folder
  needs neither.

## Consequences

- Claude subscription use is possible from the trusted CLI with the same tool
  policy, approvals, refusals, and journals as every other provider.
- Each turn pays a process start. Claude's prompt cache is not reused across
  turns except through Anthropic's own prefix caching.
- The child's network is not limited to Anthropic's hosts, and on Linux it
  can connect to any pathname Unix socket the user can reach. The child runs
  Anthropic's binary with no tools. The sandbox mainly limits what the
  binary can read and write.
- On Windows the container can read every file in the program's folder.
  A `claude` update that moves the program to another folder makes the
  recorded program stale; the connection is then refused until it is
  revoked, removed, and recorded again.
- A revocation or removal waits up to 30 seconds for running turns. The
  wait proves the processes ended, not that their effects were undone: text
  streamed before the change was already shown, and tool calls that ran
  before it stay journaled. The failed turn records no reply.
- The lockdown check follows the first model request. Tool-block checks,
  `--tools ""`, `--permission-prompts none`, and the sandbox cover that gap.
- The CLI's stream and MCP shapes are not a published stable contract. A
  `claude` update can break the adapter, and the lockdown check turns most
  such changes into a refused turn.
- A fake `claude` drives the whole path in the tests, on Windows inside the
  connection's real AppContainer with real grants: the confined launch, the
  pipe and nonce, the MCP server, the turn runtime, a revocation during a
  running turn, the CLI commands, and compaction. The same tests passed on Linux under WSL 2 (Ubuntu 24.04) in
  a delegated cgroup; the first Linux run found the socket path too long
  for `sun_path`, which decision 6 now avoids. Not yet verified: a
  signed-in turn of the real `claude` on either platform, Linux outside
  WSL 2, the desktop shell and in-turn approvals (a later change), other
  plans, rate-limit shapes, and long turns.
- On the Windows Server 2025 CI runner, a process in the connection's
  AppContainer could not open the `NUL` device (access denied), though it
  could on Windows 11. The fake gives its relay its own standard error
  instead. Whether the real `claude` opens `NUL` was not observed; if it
  does, it may fail to start on such hosts.

## References

- [Claude Code legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)
- [Claude Code authentication](https://code.claude.com/docs/en/authentication)
- [Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview)
- [MCP 2025-11-25 specification](https://modelcontextprotocol.io/specification/2025-11-25)
- [Provider authentication](../provider-authentication.md)
- [ADR-0002](0002-require-subscription-authentication.md),
  [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md),
  [ADR-0099](0099-recover-from-rejected-tool-calls-and-raise-write-tool-argument-limits.md),
  [ADR-0107](0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)
