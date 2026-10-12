# Runtime boundary maintenance

The Local Runtime owns finite local transport resources. Provider credentials and
Retry-After are request/transport policy; they do not define a process-wide
supplier quota. Boundary repairs and parser substitutions are reviewed separately.

## Scope and verification

Model HTTP contracts and byte limits are documented in
[Model HTTP](../reference/ModelHTTP.md). RPC envelope and local connection rules
are documented in [Runtime protocol](../reference/RuntimeProtocol.md) and
[command reliability](../architecture/RuntimeCommandReliability.md).

Tests use loopback servers, controlled transports and paused time. No real model
credentials or supplier calls are needed. Regression tests cover invalid input,
finite buffers, cancellation and failure cleanup; they do not lock source names.
The portable Rust gate and focused Core query-loop gate remain required.

## RPC lifecycle control admission

Request capacity and shutdown eligibility are separate policies. Ordinary reads
share the ordinary request lane. Only initialization, AgentRun cancel/detach/
detach-viewer, app exit, Runtime shutdown and sidecar stop use reserved lifecycle
control capacity. Reads remain legal during shutdown drain, but do not consume
that reserved capacity; the finite control lane can still exhaust on control traffic.

A real duplex-connection regression holds eight read handlers pending, confirms
that all were dispatched, and verifies a cancel request and its response while
those reads remain active. The test isolates business handlers to avoid store-lock
interference; framing, command classification, admission and writing are real.
Existing shutdown and active-action rules remain unchanged.

## Model HTTP Client ownership

The Runtime Server hub owns the lazy model HTTP connection pool across runs;
request transports retain their own session headers and cooldown. A new Runtime
owner creates an independent pool. See [Model HTTP Client lifecycle](../reference/ModelHTTP.md#runtime-owned-connection-pool)
for proxy sampling, validation order and failure/retry behavior.

## Atomic file publication

Temporary files are created exclusively in the target's directory using tempfile.
A failed creation never grants ownership of an existing path. Write, sync and
replacement failures clean only the temporary file owned by the current operation.
Successful publication disarms temporary-path cleanup.

Content is written completely and synced before replacement. Windows retains
MoveFileExW replacement and write-through flags, after closing the temporary file
and clearing its temporary attribute. Unix retains the previous creation mode
(0666 subject to umask). This does not add a parent-directory fsync guarantee.

Concurrent publication may choose either complete writer payload. Windows can
reject a target replacement while another operation holds the path; this remains
an explicit failure rather than an automatic retry. Tests check the old target
survives failures, other writers' temporary files survive creation conflicts, and
no owned temporary files remain after successful or failed publication.

## Parser replacement acceptance

An SSE library must preserve bare CR, CRLF and LF, fragmented UTF-8 and a leading
BOM, strict invalid-UTF-8 rejection, comments mapped to provider-waiting events,
and the existing incomplete-EOF error. Raw-byte activity controls idle timeout.
The generic decoder does not infer provider-specific termination from [DONE].

The evaluated tokio_sse_codec 0.0.2 source does not preserve bare CR or strict
unknown-field UTF-8 behavior. It is not adopted. This decision applies to that
candidate/version; it is not a claim that all parser libraries are unsuitable.
A replacement must reduce maintenance without another handwritten parser around it.
