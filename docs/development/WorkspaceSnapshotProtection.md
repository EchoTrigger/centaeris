# Workspace snapshot protection inventory

`inspect_workspace_protection` reports existing references for one Session. It
reads database metadata only: it does not open snapshot payloads, enumerate
storage, verify file existence/integrity, acquire a deletion fence, or delete
objects. It is an operator diagnostic, not a new public API.

Run from the repository root using the intended API database configuration:

```sh
uv run --frozen --package api python packages/api/manage.py inspect_workspace_protection --session-id <session-id> --max-records 1000
```

The command emits one JSON document with schema
`workspace.snapshot_protection.v1`, `sessionId`, `knownRoots`, `unresolved`, and
`deletionAuthorized: false`. Each known root includes its internal storage key,
source kind, source ID, and Run ID when applicable. It omits file bodies,
authorization payloads, signatures, and credentials.

The current known sources are:

- `sessionHead`: the current Session workspace descriptor and matching key.
- `runAuthorization`: the workspace version frozen in each recorded Run grant.
  Completed, failed, and cancelled Runs remain included. A terminal Run status
  does not prove that recovery, an uncertain commit, or audit retention is done.
  These are recorded references, not a substitute for execution authorization
  signature validation.
- `recoveryCheckpoint`: the exact workspace object named by a recovery
  checkpoint, matched to its Session, Run, checkpoint ID, and recorded grant
  digest. The Runtime Session-workspace reference
  `session-workspace:generation:<n>` is resolved to the same Run grant's frozen
  snapshot key only when its complete descriptor matches; legacy execution-
  checkpoint objects retain their exact separate keys. Checkpoint status alone
  is not used to discard a reference.

Checkpoint records must have the complete closed Core
`RuntimeRecoveryCheckpointV1` wire shape, including `executionId`,
`sessionSequence`, `modelRequestId`, `workspaceGeneration`, and `createdAtMs`.
The observer rejects missing/unknown or duplicate JSON fields, invalid identity
strings, zero sequence, negative/out-of-range timestamps, invalid unsigned
snapshot counts/sizes, and invalid `known`/`unknown` generation variants. Empty
snapshots use a null object reference and entirely empty metadata; Core also
accepts an omitted optional `objectRef` on input, which the observer normalizes
to null. A blank string is not an empty reference. Known generation epochs follow Core's 160
UTF-8 byte and control-character constraints.

The shared raw-JSON corpus is
`packages/core/tests/fixtures/recovery_checkpoints.json`. Core's
`recovery_checkpoint_contract` integration test uses the real serde and validate
entry points; the Python test resolves the same corpus with
`core_test_fixtures.core_fixture`. Valid inputs are also compared with Core's
canonical serialized shape, accounting explicitly for an omitted optional
`objectRef` becoming null. This supplements the observer's reference-scope tests
without introducing a second recovery decision.
This is shape and reference-scope validation only. It does not verify a committed
Session frontier, consumption of a recovery attempt, the tool ledger, host
availability, or whether Core would permit execution recovery. Such records are
reported as retaining references, never as executable recovery permission.
Multiple retaining sources for one key are reported separately. Empty workspace
versions have no byte object. Malformed or foreign references produce an
unresolved reason instead of an inferred storage key.

`--max-records` is a per-source history budget, defaults to 1000, and accepts
1 through 10000. The authorization query uses batched iteration; both history
queries fetch at most one extra record to detect truncation. Truncation produces
`runAuthorizationsTruncated` or `runtimeCheckpointsTruncated`. Increasing the
budget changes this observation, not deletion authority. The budget limits
record counts, not the size of each stored record.

Every report currently includes these coverage gaps:

- `concurrentReferencesNotFenced`: observations are not one fenced snapshot of
  concurrent Session, grant, and checkpoint changes.
- `inflightWorkspaceUploadsNotIndexed`: historical uploads before the publication
  index and cutover cannot be reconstructed completely. New requests use the
  durable publication index described below.
- `historicalPinsNotIndexed`: a complete history/pin retention policy has not
  been indexed by this inventory.
- `preparedCompletionsNotIndexed`: prepared completion and unresolved ownership
  obligations have not been fully enumerated.

An unavailable Runtime checkpoint table adds `runtimeCheckpointsUnavailable`.
Known API roots remain visible. Invalid records add a source-specific unresolved
reason. A key absent from `knownRoots` is therefore **not** proven unreachable.
Do not feed this report into active-generation deletion. The existing collector
for permanently purged owners retains its separate scope and now checks pending
or staged publication duties before claiming deletion attempts and before deleting bytes.

Focused validation uses the dedicated PostgreSQL test endpoint configured by
`TEST_POSTGRES_*`; it does not use deployed database settings:

```sh
uv run --frozen --package api python packages/api/manage.py test app_core.test_workspace_snapshot_protection app_core.test_workspace_snapshot_gc --settings=api.test_settings --noinput
```

Coverage includes a head advancing beyond a frozen grant, terminal grant
retention, complete checkpoint schemas and scalar ranges, exact reference scope, malformed
persisted records, unavailable Runtime reads,
closed record budgets, explicit truncation, command JSON, and read-only database
and storage behavior. This validates inventory behavior, not active-generation
GC, storage migration, real provider recovery, or resource savings.

Durable snapshot publications
-----------------------------

New Session commit and execution-stage requests now reserve a
`WorkspaceSnapshotPublication` before creating a temporary name or copying
bytes. Its protected foreign keys bind the Session, Run, and exact recorded
authorization. The immutable identity includes purpose, generation or checkpoint
identity, key, encoding, hash, sizes, file count, and a canonical request digest.
Current streaming endpoints accept `frameV1`; this change adds no encoding alias.
A checkpoint identity accepts one exact descriptor. Session commit retries may
capture another candidate before head commit, and each candidate retains its own
immutable responsibility.

The state transitions have different ownership meanings:

- `pending`: publication outcome remains unresolved, including a failed final
  transaction after the immutable name became visible. Neither request timeout
  nor object age releases this responsibility.
- `staged`: execution snapshot bytes were validated and published. Runtime has
  not transactionally acknowledged a checkpoint pin through this index, so the
  publication continues retaining the object even after the Run becomes terminal.
- `committed`: Session head and its complete descriptor were written in the same
  transaction as the publication confirmation. PostgreSQL verifies the exact
  head, Run, and descriptor during this transition. A lost COMMIT reply is
  resolved from this durable state and exact replay does not advance the head
  again. Later head advancement does not change the recorded fact.

The observer reports pending and staged keys as `snapshotPublication` roots.
Committed publications remain immutable handoff evidence and do not themselves
retain old versions: Session head, frozen Run grants, or checkpoints supply any
remaining references after head advancement. Pending or staged entries also report
`snapshotPublicationUnconfirmed:<id>`; they are retaining responsibilities, not
claims that bytes exist or that Core allows recovery. Empty snapshots retain
metadata but have no byte root. The history limit also applies independently to
publications, producing `snapshotPublicationsTruncated`. Unavailable or invalid
publication records remain explicit unresolved reasons.

The existing permanent-owner-purge collector checks publication duties under
its Agent then Session fence. Pending and staged keys, and temporary names in
their precise directory, are blocked. A committed Session publication has
completed its ownership handoff and follows the existing explicit permanent
purge contract. Active Sessions and restorable trash remain excluded. The
observer still always reports `deletionAuthorized: false`; no active-generation
collector, automatic pin expiration, or eviction permission is introduced.

Writers commit the pending intent first, then reacquire Agent, Session, Run,
current Job, and Publication in that order before making temporary names.
Each responsibility transaction requires PostgreSQL fsync and sets transaction-local synchronous_commit=on; disabled fsync rejects before temporary creation or payload copying. The caller's session policy is preserved.
Streaming, hashing, and file fsync occur outside owner transactions. Final link,
directory fsync, descriptor/head update, and confirmation share a fresh owner
transaction with the existing current lease recheck. Recovery checkpoint writes
and prepared completion semantics remain owned by Core and Runtime.

Migration `0027_workspace_snapshot_publication` preserves all existing owner,
head, grant, and checkpoint records and creates no inferred historical success.
Old unindexed writes cannot be reconstructed from filenames or modification
times. The existing coverage gaps remain conservative, including
`inflightWorkspaceUploadsNotIndexed` for that legacy/cutover gap,
`historicalPinsNotIndexed`, and `preparedCompletionsNotIndexed`. Artifact/library
publication and copying keep their current separate contracts; they are not
snapshot aliases and this change does not make their retention indexes complete.

Focused PostgreSQL/temp-storage tests in
`app_core.test_workspace_snapshot_publication` exercise visibility before
streaming and temporary creation, stage/replay conflicts, failed final commits,
lost responses, empty snapshots, owner/SQL identity constraints, bounded
metadata-only observations, explicit purge handoff, migration preservation, and
GC/publisher interleavings. They do not establish hosted-provider acceptance.
