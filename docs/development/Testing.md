# Testing

## Test quality

Tests protect observable behavior and documented contracts, not the spelling of
the current implementation. A refactor that preserves behavior should not need
test changes merely because an internal function, type, variable, CSS class, or
file was renamed, moved, or split.

Removing an implementation is not a reason to add its names to a source
blacklist. For example, replacing a history reader should be protected by the
actual request sequence, pagination, reconnect, and failure behavior, rather
than by searching source files for the former reader's name. Likewise, removing
a UI component should be verified through the resulting interactions and
visible output, not by requiring its old file to remain absent forever.

Negative assertions remain appropriate for real contracts: unauthorized requests
must fail, credentials must not appear in output, unsupported protocol fields
must be rejected, and Core must not depend on a storage adapter. State the
contract being protected. Prefer dependency graphs, parsed structure, generated
registries, and public protocol results over source text matching. A comment
mentioning a prohibited dependency must not be mistaken for an actual import.
Static checks supplement behavioral coverage; they do not establish that a
feature works merely because certain source text is absent.

Choose UI scenarios from observable risks such as wrapping, clipping, keyboard
navigation, zoom, localization, and layout transitions. Viewport dimensions are
test inputs, not correctness criteria. Assert useful relationships such as
visibility, alignment, and containment rather than incidental class names or
exact pixel values unless those values are an explicit product contract.

Before replacing a brittle test, identify equivalent existing coverage or add
the smallest missing behavioral test. Verify that a representative broken
behavior fails the replacement, and that a harmless implementation change does
not. Retain security and public-contract checks when consolidating tests. Do not
implement this policy as another blacklist of source patterns in tests; enforce
it through review and evidence.

After closing a PostgreSQL client, observe backend and advisory-lock removal
with a bounded deadline: client close does not synchronize `pg_stat_activity`.
Continue asserting both counts reach zero, and use a retained-connection
negative control to prove that a real leak still fails.

Keep unrelated outbound calls outside controlled timeout fixtures. The real
Core/HTTP admission-response tests isolate return-job scheduling, whose real
HTTP/store recovery has separate coverage; a closed port's platform-dependent
connection delay must not determine whether the response-loss window is reached.
Upload alias tests retain their no-refund assertion on both platforms: Unix uses
a dangling symlink, and Windows uses a dangling directory junction without
requiring the symlink privilege. Neither case skips the resource-safety check.

HTTP transport refactors first characterize actual request dispatch, rejection
order, response envelopes/headers and cancellation/timeout resource ownership.
Protocol-function and storage tests remain necessary but do not substitute for
the complete HTTP path. For hosted Runtime endpoints, use the protected
behaviors and coverage gaps in [Runtime HTTP](../workspace/reference/RuntimeHTTP.md).

For Django schema refactors, compare the generated OpenAPI and both validation
and serialization schemas before and after, in addition to request tests.
`test_schema_wire_contract.py` covers nested camelCase fields, strict types,
exceptional aliases and unset/null/false distinctions. `test_http_error_contract.py`
exercises shared-path method routing and public error statuses through Django
and Ninja. `test_runtime_http_client.py` uses a loopback HTTP server to check
request bytes, operation-specific errors and absence of automatic retries.
Schema parity does not replace authorization, persistence or cancellation tests.
For loopback HTTP/HTTPS fixtures, isolate inherited proxy settings in the test
process. `NO_PROXY` alone may not prevent HTTP clients from initializing optional
proxy transports; do not change machine-wide proxy settings to run these tests.

Keep domain effects under their existing real store/provider tests; add only the
smallest observation seam needed for controlled blocking or write detection.

Hosted model gateway tests exercise real SDK requests with synthetic HTTP
responses: credentials work without an application quota binding, upstream
429/503 metadata remains available, and subsequent requests are neither retried
nor delayed by application-wide provider cooldown. The migration test preserves
encrypted credentials, model configuration and application capacity records.
Real HTTPS/ASGI tests retain coverage for ORM connection return, caller-owned
transactions and stream cancellation. See the
[hosted model gateway boundary](../workspace/architecture/HostedModelAdmission.md).

## Focused Core gate

Every Rust change runs at least:

```powershell
cargo test --locked -p centaeris-core query_loop
```

Add the smallest focused test owned by the changed contract or adapter. Core
tests runtime semantics; adapter tests cover storage, transport, and platform
integration.

## Full local gate

```sh
python3 scripts/ci.py Source --frontend-tests
```

On Windows use `python`. Windows distribution acceptance additionally runs
`pwsh -File scripts/ci.ps1 Release`.

The gate checks formatting, the Rust workspace, Clippy with warnings denied,
focused Core and SQLite integration, all Rust tests and Desktop/UI source checks.
Packaging and packaged-application smoke checks remain in the Windows release stage.

## Test data

Tests must use temporary roots and synthetic credentials. They must not read a
developer's real `~/.centaeris`, production data, private extension content, or
customer files. A test result is evidence for the exact tested source tree; it
is not a source artifact and should not be committed.

## Clean-clone gate

Before release, run the same gate from a clean clone of the exact candidate
revision. A passing dirty working tree does not prove that ignored files,
adjacent repositories, or previously built binaries are unnecessary.

Work-return publication changes protect three behavior boundaries: history-independent normal job traffic; shared audit leasing, restart cursors and stale-owner rejection; and omitted/unknown schedule repair without losing source records. The immutable previous publisher characterization and history-count RED establish the replaced behavior; worker/API tests cover deadlines, partial progress, cancellation and transport outcomes. No source-name absence checks enforce this change.
