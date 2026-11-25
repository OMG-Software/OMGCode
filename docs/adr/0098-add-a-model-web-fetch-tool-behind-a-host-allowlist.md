# 0098: Add a model web fetch tool behind a host allowlist

- Status: Accepted
- Date: 2026-11-25
- Amends: [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)

## Context

An operator asked ChatGPT, through OMGCode, to read two API documentation
sites. The model replied that the session had no web access. That was
correct. The model tools read, list, and write workspace files and run one
command, and `workspace_run_command` always runs with networking off. The
[execution guide](../execution.md) listed model network tools as later work
that needs its own decision.

A fetch tool is the widest channel any model tool has had:

- The model chooses the URL. A path or query string can carry workspace data
  to the server that receives it.
- The fetch runs on the operator's computer, which can reach loopback
  services, the local network, and cloud metadata endpoints. A name that
  resolves to a public address once can resolve to a private one later.
- A web page is text chosen by a third party. It is the most likely carrier of
  prompt injection.

The tool must work with every provider, so it cannot depend on one
provider's hosted search or browsing tool.

## Decision

### The tool

`web_fetch` takes `url`, an optional `offset` into the page text, and an
optional `max_bytes` (default 12 KiB, at most 15 KiB). OMGCode sends one HTTPS
`GET` from its own process and returns a short header and one slice of the
page text. The model reads a longer page with further calls at later offsets.
Each call fetches the page again. The tool schema version is `0.8.0`, after
the `0.7.0` of [ADR-0099](0099-recover-from-rejected-tool-calls-and-raise-write-tool-argument-limits.md),
and a model request and a session journal may carry ten tool definitions.

### The gate

- A new workspace preference, `web_fetch`, is `deny`, `prompt`, or `allow`.
  It defaults to `deny` and is omitted from stored settings while it is
  `deny`, so existing settings keep their bytes. With `model_tools` off the
  tool is denied. It is offered only while the preference permits it.
- Each workspace has a host allowlist of at most 32 patterns. A pattern is an
  ASCII DNS name, which matches only that host, or `*.` followed by a name,
  which matches its proper subdomains only. Only the trusted CLI and the
  `settings` window, after a native confirmation, change it. The CLI sets
  the preference with `workspace permissions <workspace> --revision <n>
  --web-fetch deny|prompt|allow` and changes the list with `workspace web-host-add` and
  `workspace web-host-remove <workspace> <pattern> --revision <n>`;
  `workspace web-host-list <workspace>` prints it.
- Under `prompt`, every fetch needs a trusted single-use approval for the
  exact URL, offset, and size, whatever the host.
- Under `allow`, a fetch to an allowlisted host runs without an approval. A
  fetch to any other host needs one, as under `prompt`. Workspace data can
  therefore reach an unlisted host only after the operator has read the whole
  URL.
- Approvals use the `network` operation, which was reserved and unused. The
  request digest uses the domain `omgcode-web-fetch-v1`. The approvals window
  shows the URL, its host, and whether the host is allowlisted. No
  writers-stopped acknowledgement is needed.
- The model action journal records each call as `network`, with the
  normalised URL as its target.

### The network policy

- The URL must use `https` on port 443, carry no user name or password, be at
  most 2048 bytes, and name a DNS host. IP literals, `localhost`,
  single-label names, and local-use suffixes such as `.local` and `.internal`
  are refused. The fragment is dropped.
- Every resolved address is checked. If any address is not global unicast
  (loopback, private, shared, link-local, multicast, documentation,
  benchmarking, reserved, NAT64, 6to4, Teredo, unique-local, or IPv4-mapped),
  the fetch is refused. The client connects only to the addresses that were
  checked, so a later DNS answer cannot move the connection.
- The client uses no proxy, cookies, or credentials and sends no `Referer`.
  It sends a fixed user agent and `Accept-Encoding: identity`, and refuses any
  other encoding. It verifies certificates, requires TLS 1.2 or later, and
  allows 10 seconds to connect and 30 seconds for the whole fetch.
- A redirect to the same host is followed, at most five times, and each hop is
  checked again. A redirect to another host is not followed. The result names
  the target, so the model can request it and pass the gate again.
- Only a `2xx` response with an allowed text type returns text. At most 2 MiB
  of the body is read, and truncation is reported. HTML becomes text through
  an in-process converter. It drops scripts, styles, frames, embedded objects,
  and elements marked `hidden`, `aria-hidden="true"`, or hidden by an inline
  style. Control, zero-width, and bidirectional formatting characters are
  removed, and the text is capped at 1 MiB.
- The settings lock is released before any network I/O, after any approval
  is spent and the start of the call is recorded. A settings change made
  during a fetch applies from the next call; the fetch in flight is not
  stopped.

### Untrusted content

A fetch result is labelled as untrusted web content from its host, not as
workspace data. It grants no permission. Saved fetch results replay under the
same live rule as other tool results.

### An audit record after web content

The operator chose not to tighten `allow` after a fetch. Instead, every model
action that a session records after its first fetched web content carries
`after_web_content` in the model action journal, whatever its tool or
outcome. A fetch counts once the fetcher returned any result, including a
status or redirect report without page text, because the model then holds
text the server chose. The record names the tool and its workspace targets.
A `workspace_run_command` record has no target: its request digest binds the
program and arguments, which the session and run journals hold, and nothing
names the files the program touched. A session's first fetched content is found again from its journal at
the start of each turn, so the marker survives a restart and compaction. The
trusted `actions list --after-web-content` command shows only these records,
optionally for one `--session`, and the desktop's right-hand panel lists them
for the open session.

## Alternatives considered

- **A provider's hosted web search.** Rejected. It serves one provider, is
  unverified on the ChatGPT plan route, and gives OMGCode no control over the
  destination.
- **Reusing `task_network`.** Rejected. That preference lets trusted runs use
  the network, and a networked Linux run reaches every destination. An
  existing `allow` would have silently enabled model fetches.
- **Fetching through `workspace_run_command` with networking.** Rejected. A
  program cannot be held to one URL, and a networked Linux run reaches
  loopback and the local network.
- **Any public host under `allow`.** Rejected by the operator. A
  prompt-injected model could send workspace data to any server.
- **Allowlisted hosts only.** Rejected by the operator. Under `prompt` any
  public host can still be fetched after an approval.
- **Lowering `allow` to `prompt` for writes and commands after a fetch.**
  Rejected by the operator in favour of the audit record.
- **An HTML parser crate.** Rejected. It adds a large dependency tree for
  bounded text extraction.
- **Following redirects to other hosts.** Rejected. The destination would
  bypass the gate.
- **Allowing `http` or IP literals.** Rejected. `http` has no server identity,
  and literals are an easy route around the address policy.

## Consequences

- A model can read public documentation without the operator downloading it
  first.
- Data can still leave through a URL to an allowlisted host under `allow`, or
  through any URL the operator approves. The approval shows the whole URL.
- Hiding detection is heuristic. Text hidden by a stylesheet class, placed off
  screen, or written in visible elements still reaches the model.
- No script runs, so a page built by JavaScript returns little text.
- An operator whose network requires a proxy cannot use the tool.
- A wildcard is not checked against the public suffix list, so `*.co.uk` is
  accepted.
- Older OMGCode versions refuse a settings log that holds a `web_fetch` state
  or a host pattern, and an action journal that holds `after_web_content`.
- Automated tests cover the policy, the converter, and the client against a
  loopback server under a test-only policy. They do not fetch a real site,
  and no test makes a TLS handshake fail, so the `tls_failed` code is inferred
  from the client's error and is unverified.

## References

- [Workspace permissions](../workspace-permissions.md)
- [Execution guide](../execution.md)
- [Security coding rules](../security-coding-rules.md)
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0079](0079-add-a-model-tool-that-creates-one-directory.md)
