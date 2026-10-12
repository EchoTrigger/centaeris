# Local model HTTP boundaries

The Local Runtime uses reqwest for JSON and SSE model requests. Automatic HTTP
redirects are disabled for both paths: provider API keys and session headers are
bound to the configured endpoint, including redirects back to the same host.
Non-success SSE responses retain their status, headers and bounded body for the
existing protocol adapters. Successful SSE responses must have the
`text/event-stream` media type (parameters are accepted).

## Runtime-owned connection pool

Each Runtime Server owns one lazy model HTTP Client, shared by main AgentRuns,
manual compaction, model connectivity checks and background subagent requests.
It is built on the first valid model transport construction and only successful
construction is cached. Failures remain model-operation errors and a later
operation can retry construction. A new Runtime Server owner gets a new pool;
there is no process-global Client singleton or network configuration registry.
Dropping the hub releases its cached Client clone; existing transport/in-flight
clones retain their own lifetime. It does not forcibly terminate HTTP operations.
Service drain and caller cancellation retain their existing responsibilities.

The Client is header-neutral except for the product User-Agent. Credentials and
OpenCode session headers are request-local. OpenCode session validation still
runs before obtaining/building the shared Client; other providers do not gain
that validation. Raw session header values are preserved. Explicit session or
User-Agent request headers override defaults case-insensitively, and multiple
explicit case variants are not collapsed. Sharing a connection pool does not
share a transport's Retry-After cooldown: each transport keeps its own cooldown
and the existing shared local request-capacity admission.

The current locked Local Runtime uses environment-derived proxies, including
HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, NO_PROXY and their lowercase variants; reqwest's
CGI-related environment handling is unchanged. Windows/macOS system proxy lookup
is not enabled in this dependency feature graph. Proxy settings are sampled on
the first successful Client construction and remain fixed for that Runtime
Server. Updating its inherited environment requires a new Runtime Server;
reconnecting a Desktop/TUI client to an already running service does not refresh
that service's environment. Model/config reset does not reset network settings.
There is currently no in-process proxy update API. TLS validation, SOCKS support,
redirect rejection and request-specific timeouts remain unchanged.

Keepalive tests count actual accepted TCP connections across different session
transports and mixed JSON/SSE exchanges, using finite Content-Length bodies while
the server keeps TCP open. Additional tests cover concurrent fake-credential and
session isolation, duplicate explicit headers, lazy validation/error ordering,
success-only caching and independent pools for different Runtime owners. These
establish connection reuse and behavior, not a measured latency/throughput gain.
The model test `latencyMs` remains elapsed diagnostic wall time from the operation
entry, including configuration, admission and request processing. It may now use
a warm connection pool; it is not an isolated cold-connection or inference metric.

## Response memory budgets

JSON bodies and non-success SSE bodies have a 16 MiB wire-byte ceiling. SSE has
separate 16 MiB ceilings for one line and accumulated event data, including the
newlines joining multiple data fields. The line and event ceilings use the same nominal budget, avoiding a separate
restrictive line policy. The line budget includes field syntax; the event budget
counts decoded data and joining newlines. An entire long-running stream has no cumulative byte limit.

These are explicit local transport safety ceilings, not supplier quotas or
measured hardware capacity. There was no pre-existing maximum model response
contract to derive a precise value from. 16 MiB is a conservative envelope with
substantial room for text and structured tool output; an endpoint needing larger
individual responses requires a deliberate contract/budget update. Four active
attempts can each retain bounded line/event/body buffers; these ceilings do not
bound memory retained later by model protocol adapters or application consumers.

Overflow fails the transport without delivering a partial event. A declared
oversized Content-Length is rejected before buffering; chunked bodies are also
checked during actual reads. Existing request total timeout for JSON and header
wait for SSE remain; non-success SSE body timeout remains a total read deadline.
Successful SSE idle timeout surrounds each raw byte read, so continued chunks
reset it even if they have not completed an event.

## SSE compatibility and replacement decision

One leading UTF-8 BOM is ignored, including when split across network chunks.
LF, CRLF and bare CR delimiters, multiline data, split UTF-8, comments mapped to
provider-waiting events, and errors for incomplete EOF remain covered by tests.
`[DONE]` is suppressed as data but does not terminate the generic transport:
providers have different completion protocols. A stream sending `[DONE]` without
EOF still reaches the existing idle timeout. Protocol-specific early termination
requires a separate explicit contract change.

The parser-only candidate `tokio_sse_codec` 0.0.2 was inspected against these
contracts. Its field decoder searches for LF, accepts CRLF, but does not accept
bare CR line termination; it also decodes unknown fields lossily while the local
transport rejects invalid UTF-8 in every line. Adapting these differences would
require another line-normalization/validation layer, so it is not adopted here.
Its comment frames, BOM handling and bounded buffers alone are insufficient to
establish equivalent behavior. No request/reconnect library is introduced.

Focused verification lives in `http_transport` tests, using loopback HTTP servers,
byte-split parser tests and paused-time idle-timeout tests. No real credentials
are used. Tests cover redirect target isolation for JSON/SSE, response media type
and non-success classification, body/line/event overflow and BOM boundaries.

Candidate source and documentation:
- <https://docs.rs/tokio_sse_codec/0.0.2/tokio_sse_codec/>
- <https://docs.rs/crate/tokio_sse_codec/0.0.2/source/src/field_decoder.rs>
- <https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream>
