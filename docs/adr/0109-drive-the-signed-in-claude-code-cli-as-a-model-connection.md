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
   absolute path of the operator's `claude` program and the operator's
   Claude configuration folder, each with its recorded identity. It records
   no endpoint, account, token, or credential binding. The profile is
   configuration only: it does not show that Anthropic approves the route.
2. **The operator signs in, and OMGCode stays out of it.** The operator runs
   `claude auth login` with `CLAUDE_CONFIG_DIR` set to a folder used only for
   OMGCode. OMGCode has no Claude sign-in command. It never opens, reads,
   copies, stores, or passes a file or token from that folder. It only sets
   `CLAUDE_CONFIG_DIR` for the child and, on Windows, grants the child's
   container access to the folder. `claude setup-token`,
   `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and
   `ANTHROPIC_AUTH_TOKEN` are never set, so the CLI uses only its own
   sign-in. A dedicated folder keeps this sign-in's refresh-token rotation
   apart from the operator's everyday Claude Code sign-in.
3. **A dedicated sandbox per connection.** On Windows, the child runs in its
   own AppContainer, `OMGCode.ClaudeCode.` followed by the profile
   identifier. The container has the `internetClient` capability. It gets
   read-execute on the program's folder, the OMGCode runtime folder for the
   connection, and the relay program's folder, and read-write on the
   configuration folder. Each grant is recorded before and after the ACL
   change, the same way workspace grants are, and removing the profile
   revokes them. A workspace container is never used: a model-run command
   there must not reach the sign-in folder, and the workspace container must
   not gain a network capability. On Linux the same paths go into Landlock.
   The seccomp filter allows IP sockets and `AF_UNIX`
   (`NetworkAccess::Internet` with `UnixSocketAccess::HostServices`), and
   the run has its own cgroup with the usual tree limits. Neither platform
   limits the child to Anthropic's hosts.
4. **One process per OMGCode turn, launched locked down.** Every turn starts
   one `claude` process. Its environment is built from nothing, with only
   these variables:
   - the platform basics: `SystemRoot`, `windir`, `LOCALAPPDATA`, a container
     `USERPROFILE`/`HOME`, and on Linux `TMPDIR`;
   - `CLAUDE_CONFIG_DIR`;
   - `DISABLE_AUTOUPDATER=1`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`,
     `DISABLE_TELEMETRY=1`, and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`;
   - MCP timeouts derived from the turn deadline.

   The arguments are `-p`, `--output-format stream-json`, `--verbose`,
   `--include-partial-messages`, `--tools ""`, `--strict-mcp-config`,
   `--mcp-config <file>`, `--setting-sources ""`, `--settings <file>` (built-in
   plugins disabled), `--system-prompt-file <file>`,
   `--no-session-persistence`, `--disable-slash-commands`,
   `--allowedTools mcp__omgcode__*`, `--permission-prompts none`,
   `--permission-mode dontAsk`, `--model <model>`, and `--effort <effort>`
   when the session has an effort. The working folder is an empty folder in
   OMGCode's private storage. The prompt goes to stdin as text, and stdin is
   then closed.
5. **Lockdown is checked, and it fails closed.** The `system/init` event must
   satisfy all of these, or the tree is terminated and the turn fails with
   `claude_code_lockdown`:
   - exactly the offered tools, named `mcp__omgcode__<tool>`;
   - exactly one MCP server, `omgcode`, with status `connected`;
   - no plugins, skills, or slash commands;
   - `apiKeySource` `none`.

   Because that event follows the first request, each tool block is checked
   too. A block that names anything other than an offered tool terminates the
   tree at once, from its first streamed `content_block_start`.
6. **Tools come back through a relay over a local pipe.** The MCP
   configuration starts `omgcode mcp-relay` as the only server. The relay
   runs in the same sandbox and copies bytes between its stdio and one local
   channel. It sends a per-turn nonce first. On Windows the channel is a
   named pipe that admits the user and the connection's container. Its
   mandatory label is Low with no-write-up, and the server accepts a client
   only after the client's token shows that container's SID. On Linux it is
   a socket in a 0700 per-turn folder, with a same-user peer check. All MCP
   handling stays in the trusted process. It answers `server/discover` with
   `-32601` so the observed client falls back to `initialize` at
   `2025-11-25`, offers only `tools`, and never sends a request to the
   client.
7. **The adapter is an ordinary `ToolModel`.** `ClaudeCodeModel` implements
   the core `ToolModel` trait. On a round it returns each pending
   `tools/call` as a tool call: the identifier is its `claudecode/toolUseId`,
   the name is the MCP name, and the arguments are re-serialised. A round
   holds at most eight calls; any others wait for the next round. The MCP
   request stays open while the turn runtime parses the call, applies the
   workspace policy, waits for any approval, dispatches, filters, and
   journals it, exactly as for every other provider. The next round answers
   each held request with the result text. The `result` event ends the turn
   with its final text. No tool runs outside the runtime's dispatcher, and
   claude's own permission system decides nothing.
8. **OMGCode's journal is the only history.** Because each process lives for
   one turn with `--no-session-persistence`, OMGCode renders the earlier
   conversation into the first prompt. The labelled history text is the same
   that compaction and replay send. Within a turn, claude keeps its own
   context. Compaction summaries for this connection go through the same
   launch with no tools.
9. **Failures are named.** A failure that the launch, sign-in, lockdown,
   protocol, or child exit causes is reported as one of
   `claude_code_not_signed_in`, `claude_code_lockdown`,
   `claude_code_protocol`, or `claude_code_failed`. Cancellation and the turn
   deadline terminate the tree, and its end is observed before the turn
   returns.
10. **Terms and readiness.** The connection is personal use of the operator's
    own sign-in to the unmodified binary. OMGCode offers no Claude.ai sign-in
    and stores, reads, and brokers no Claude credential. It is not evidence
    of Anthropic approval. The release requirement for Claude subscription
    support stays open until Anthropic approves the route or documents a
    supported one.

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

## Consequences

- Claude subscription use is possible from the trusted CLI with the same tool
  policy, approvals, refusals, and journals as every other provider.
- Each turn pays a process start. Claude's prompt cache is not reused across
  turns except through Anthropic's own prefix caching.
- The child's network is not limited to Anthropic's hosts, and on Linux it
  can connect to any pathname Unix socket the user can reach. The child runs
  Anthropic's binary with no tools. The sandbox mainly limits what the
  binary can read and write.
- The lockdown check follows the first model request. Tool-block checks,
  `--tools ""`, `--permission-prompts none`, and the sandbox cover that gap.
- The CLI's stream and MCP shapes are not a published stable contract. A
  `claude` update can break the adapter, and the lockdown check turns most
  such changes into a refused turn.
- Not yet verified: a turn inside the AppContainer, Linux, desktop
  selection and in-turn approvals (a later change), other plans, rate-limit
  shapes, and long turns.

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
