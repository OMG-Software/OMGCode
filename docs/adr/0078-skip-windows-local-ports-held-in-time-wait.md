# 0078: Skip Windows local ports held in TIME_WAIT

- Status: Accepted
- Date: 2026-11-23

## Context

On Windows, a tool-enabled chat turn against LM Studio failed with
`model_error` straight after a tool result. The next model request never
reached the server. LM Studio logged no request, and the turn ended about a
millisecond after the tool completed. The underlying `reqwest` error was a
connect failure with `WSAEADDRINUSE` (10048).

`hyper-util` 0.1.21 binds every client socket to the unspecified address and
port 0 before it connects on Windows (`client/legacy/connect/http.rs`,
`bind_local_address`, commented "Windows requires a socket be bound before
calling connect"). `mio` 1.2.4, which `tokio` uses, connects without that
bind, so the bind is not required. Windows then chooses the local port before
it knows the destination. On the development host it returned the lowest free
port, and a port whose earlier connection to the same server is in TIME_WAIT
counts as free. A later probe showed that the bind is not the only route: a
plain connect with no bind first, as `std` and `mio` make, was refused the
same way in 39 of 40 attempts after client-closed connections to one
listener. The connect to that server is then refused for the
four-tuple, until the TIME_WAIT entry expires two to four minutes later.

The client usually holds that TIME_WAIT itself. Each model request builds its
own runtime and client and closes the connection when it returns, before the
server does. A desktop model-list refresh one second before the failing
request left the same kind of entry. In a Python reproduction of the
bind-then-connect sequence, every attempt after one client-closed connection
was refused on port 49157. When the server closed first, six consecutive
attempts on that port succeeded.

## Decision

Retry a model, model-list, or trusted package-fetch request whose connect is
refused with `WSAEADDRINUSE` on Windows. Before each retry, bind one
unconnected socket for each address family to port 0 and hold it until the
request is sent. The held socket is expected to take the refused port, so the
next bind chooses a later one. This is a best-effort reservation: the bound
port is not compared with the refused one, and when only one family binds,
a retry in the other family holds nothing new and can be refused again. Skip
at most 64 ports per request; then report the
failure as before: `Transport` for a model request, and the same transport
failure for a package fetch. The sockets never listen or connect, and they are
closed when the request returns. The retry is limited to this refusal, which
happens before any request byte, and so before any credential header, reaches
a server. Any other failure keeps its existing category, a cancelled model
request stops waiting as before, and the package fetch deadline bounds every
attempt. A model or model-list request sets the time left before its own
deadline as each attempt's request timeout, so a retry never extends the
request's documented limit, including reading the body; no retry starts
after the deadline. The check is compiled only on Windows. The code is in
`crates/omgcode-core/src/connect.rs`.

Send `Connection: close` on a model-list request to a plain-HTTP loopback
server. The HTTPS Ollama Cloud endpoint does not receive it: HTTP/2 forbids the
header. This build negotiates only HTTP/1.1, so the header would be valid
there today, but it has no purpose there. A server that honours it by
closing at once after its reply holds the TIME_WAIT itself, which cannot
refuse a later client connect. That close order was not observed with LM
Studio. In a loopback probe, a server that replied with `Connection: close`
but did not close saw the client close first, about 0.1 ms after the reply,
in three of three rounds. The header therefore helps only with a server that
closes promptly; otherwise the reused-port skip handles the TIME_WAIT the
client holds. A list request is a few hundred bytes, which is too small to
delay a reconnect either (see Consequences). Chat requests do not send it,
because it did not change the close order for a streamed reply (see
Alternatives).

## Alternatives considered

Sending `Connection: close` on chat requests and reading the body until the
server closed it was tried first. A socket trace showed the client still
closed each streamed connection first: `hyper` shuts the connection as soon as
the final chunk arrives. That change was removed.

Retrying without holding a socket gets the same port again on a host that
returns the lowest free port, so every retry is refused.

Keeping one client and its connection pool for a whole turn would avoid most
new connects. It reverses the per-request runtime design. It still closes from
the client at the end of each turn, so the next turn's first connect could be
refused.

Removing the bind would not fix it: a connect without a bind was refused the
same way on the development host (see Context). It is also not possible
without a fork, because `reqwest` 0.13 does not expose the connector and
`hyper-util` has no option to skip the bind.

Setting `SO_REUSEADDR` or a zero linger time on the socket is not exposed by
`reqwest`. `hyper-util` sets `SO_REUSEADDR` only after its bind.

## Consequences

Back-to-back model requests on Windows no longer failed in the tests and
live runs below because the previous connection was still in TIME_WAIT. The
reservation is best effort (see Decision), so this is observed behaviour on
one development host, not a guarantee. The new test
`consecutive_requests_survive_client_side_time_wait` uses a server that waits
for the client to close each connection. On Windows it fails when the retry is
disabled and passes with it. On other systems the retry is inert. Three
multi-round runs against LM Studio with `google/gemma-4-e4b` completed 4, 9,
and 1 rounds without a connect refusal. Before the change every second
request failed. The third run ended on an unrelated invalid tool path.

Each skipped port costs one refused local connect, about a millisecond, and
holds up to two bound sockets until the request is sent. More than 64 ports
in TIME_WAIT to the same server would still fail a request. The Windows port
choice was observed on one development host, not taken from documentation.
Chat connections still end in client-side TIME_WAIT. The retry handles them
when the next request is sent; nothing prevents them.

A TIME_WAIT that the server holds is not refused, but it can delay a connect
until the connect timeout. Windows can give the next client the same local
port, which recreates the closed connection's addresses. The server then
seems to drop the new SYN when the previous connection carried a large
request. In a loopback probe on the development host, a same-port reconnect
after a 30,000-byte request timed out, while after 2,000 bytes or less it
connected at once. The retry cannot see this case, because nothing is
refused. A model-list request stays well below that size.

The package fetch had the same failure. Each registry fixture serves one
connection, and the next fixture often received the same server port while the
client received the same local port. Twelve rounds of the package tests failed
in seven, with 18 refusals, every one `WSAEADDRINUSE`. After the change, 35
rounds passed.

Test fixtures that serve the model provider client read until the client
closes before they close, so the TIME_WAIT stays on the client side, where the
retry handles it. Package fixtures keep closing first, which is harmless after
a small request. Scripted fixtures also keep their port bound until the test
finishes, and an endpoint that must fail is a listener that closes every
connection. A released port could otherwise be bound by another test's
fixture, which would receive the request. Before these changes the CLI
`model_tools` target failed in most runs on the development host; afterwards
it passed ten consecutive runs.

The core `local_provider` target failed all 20 runs on the development host
before this branch. Waiting for the client's close cut that to 5 of 20, each a
fixture that saw no connection before the client's five-second connect
timeout. Its one-shot fixtures now also keep every port bound until the test
binary exits, so no later fixture receives an earlier one's server port; then
1 of 30 back-to-back runs failed the same way. That stall may come from a
previous run's TIME_WAIT on a port the new run's fixture received; this was
not confirmed. See the
[testing standards](../testing-standards.md#isolation-and-concurrency).

The Windows confinement test's unconfined TCP probe failed the same way: the
child closed its connection first, and the grandchild's connect to the same
host listener was refused with `WSAEADDRINUSE`. The host listener now accepts
and closes each connection, the probe waits for that close, and a refused
port is held and skipped as above. The confined tests also check that no
connection reached the host, because a refusal there used to count as a
denial. The target passed 25 consecutive runs after the change.

## References

- [ADR-0049](0049-stream-local-model-replies-and-push-chat-events.md) and
  [ADR-0066](0066-choose-the-chat-model-from-the-local-servers-model-list.md):
  the streamed chat request and the model list this applies to.
- [Microsoft: Using SO_REUSEADDR and SO_EXCLUSIVEADDRUSE](https://learn.microsoft.com/windows/win32/winsock/using-so-reuseaddr-and-so-exclusiveaddruse)
- `hyper-util` 0.1.21 `src/client/legacy/connect/http.rs`, `bind_local_address`;
  `mio` 1.2.4 `src/net/tcp/stream.rs`, `TcpStream::connect`.
