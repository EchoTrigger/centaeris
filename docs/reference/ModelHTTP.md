# Local model HTTP boundaries

The Local Runtime uses reqwest for JSON and SSE model requests. Automatic HTTP
redirects are disabled for both paths: provider API keys and session headers are
bound to the configured endpoint, including redirects back to the same host.
Non-success SSE responses retain their status, headers and bounded body for the
existing protocol adapters. Successful SSE responses must have the
`text/event-stream` media type (parameters are accepted).

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
