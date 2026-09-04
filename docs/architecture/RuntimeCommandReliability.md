# Runtime command reliability

The Local Runtime uses JSON-RPC 2.0 framing over a local JSONL transport. The
transport looks like a function call, but a caller can lose the connection or
time out after the Runtime has already committed a mutation. This page records
the current guarantees and the incremental work required to make that ambiguity
machine-visible.

The generated method inventory is
`packages/runtime/generated/runtime-methods.json`. Method names and command
scopes are generated from
`packages/runtime/src/runtime_command_registry.rs`; this page does not duplicate
that list.

## Verified current behavior

- JSON-RPC `id` correlates one response on one connection. Desktop generates
  `electron-N` and TUI generates `tui-N`; neither identity is persisted or
  accepted as a command idempotency identity.
- TUI applies a 30-second timeout to synchronous Runtime requests. Desktop
  applies a finite deadline to ordinary Runtime requests as well as bounding
  endpoint discovery and stale-Runtime shutdown.
- Neither client automatically retries a Runtime command after timeout or
  disconnect.
- Runtime errors expose a stable domain `code` in `error.data`. Desktop and TUI
  both retain that code separately from the diagnostic message. TUI also uses
  distinct local codes for request timeout, connection loss, a request that was
  not sent, and a closed response channel; timeout and connection loss mark the
  outcome as unknown.
- Desktop and TUI retain a bounded set of abandoned request ids per connection.
  The first late response for a known abandoned request is consumed, while a
  repeated late response or a genuinely unknown id still fails closed.
- Durable Session events are idempotent by event identity. `session/new` and
  `session/prompt` additionally require a caller-owned `operationId` and persist
  a request digest plus their original result before the caller depends on the
  response. Repeating the same normalized request returns the original
  identities; reusing the identity with another request fails with
  `operation_id_conflict`.
- The generated method inventory records `operationKind`, `retryPolicy`, and an
  optional `reconcileMethod` for all registered methods. Reads are the only
  commands classified `safeRetry`; only the two receipt-backed Session commands
  are classified `sameOperationId`; every other mutation remains
  `noAutomaticRetry` until behavior tests prove a stronger guarantee.
- Version 1 retains operation receipts indefinitely. That preserves exact replay
  and operation-id conflict detection, including after Session deletion, at the
  cost of storage growth proportional to accepted Session commands. A future
  retention or compaction policy must define what happens to old operation
  identities before deleting receipts; an arbitrary TTL would weaken the
  current contract.

## Mutation audit

The first audit separates operations by observable retry behavior rather than
by their names.

### Already naturally idempotent or identity-addressed

- AgentRun attach uses a viewer/session set, detach removes that binding, and
  cancel returns `cancelled: false` once the run is terminal.
- Workspace activate and rename write a desired value; remove reports whether
  an entry was present. `workspace_get` is the reconcile query.
- Runtime configuration set/reset and Plugin or Skill enabled-state changes
  write desired state and have corresponding catalog/configuration queries.
- Read, list, detail, projection, diagnostics, file preview, and Git inspection
  methods do not create durable domain identities. They may be repeated after a
  transport failure, subject to ordinary external I/O freshness.

These operations still need a finite client timeout and explicit metadata, but
they do not need a new deduplication store merely to make a repeated identical
request safe.

### Recoverable, but not response-idempotent

- `plugin/install` stages and validates a package before renaming it into the
  managed directory. Repeating the request after a successful commit returns
  “already installed” rather than the original success response; Plugin catalog
  and detail calls can reconcile the final state.
- Plugin removal, Session deletion, dead-letter transitions, garbage
  collection, sidecar lifecycle, and similar identity-addressed commands can
  usually reconcile through an existing list/detail method, but their response
  semantics are not uniformly defined for a repeated request.

### Durable request identity and remaining boundaries

- `session/new` derives one durable `sessionId` from the operation identity and
  recovers if the Session committed before its receipt. Its receipt survives
  Session deletion, so replay does not recreate deleted state.
- `session/prompt` derives one `turnId` and `agentRunId` from the operation
  identity. Its receipt is written before AgentRun startup, so retry can resume
  the receipt-before-run crash window; active, terminal, and reconnected replay
  all return the original identities without appending another user message.
- Supplement and intervention commands require an existing `agentRunId`, but
  repeated delivery semantics must be checked against their durable
  intervention identities before they can be classified as safe retries.

UI and TUI generate a fresh operation identity at each user-action boundary and
pass it unchanged through request construction. Neither client automatically
retries today; a later retry implementation must retain the same identity for
the same uncertain action rather than generating another one.

## Incremental contract plan

1. **Complete:** Give Desktop ordinary Runtime requests a finite timeout. A
   timeout must be a distinct local error that says the outcome is unknown; it
   must not trigger an automatic command retry.
2. **Complete:** Track abandoned request ids for the life of a connection so one
   late response is consumed without treating it as a protocol violation.
   Unknown response ids must continue to fail closed. Apply the same behavior to
   TUI.
3. **Complete:** Preserve Runtime domain error codes in both clients. Define
   transport timeout, connection loss, deterministic domain failure, and
   outcome-unknown as separate client-visible classifications.
4. **Complete:** Add reliability metadata to the generated Runtime method registry only after
   behavioral tests establish each classification. The minimal candidate fields
   are `operationKind`, `retryPolicy`, and `reconcileMethod`; ownership remains
   represented by command scope until a distinct owner is demonstrated useful.
5. **Complete:** Add persisted idempotency receipts first to `session/new`, then to
   `session/prompt`. Tests must cover commit followed by lost response, duplicate
   delivery, reconnect, and receipt lookup before clients are allowed to retry.
6. Generate request/response schema only for cross-language or high-risk
   boundaries as they are migrated. Independent boundary validation and shared
   golden fixtures remain required; the method registry is not a reason to
   generate 73 nominal schemas with no behavioral value.

## Required test order

Each implementation slice starts with a failing behavior test:

1. Desktop request deadline and outcome-unknown error.
2. Desktop and TUI accept exactly one response for a locally abandoned request,
   while still rejecting a genuinely unknown response id.
3. TUI preserves `error.data.code`.
4. Duplicate `session/new` with one `operationId` returns one durable Session.
5. Duplicate `session/prompt` with one `operationId` returns one `turnId` and one
   `agentRunId`, including after reconnect.

No automatic retry is introduced before steps 4 and 5 have durable receipts and
reconcile behavior.
