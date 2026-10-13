# Conditional workspace capture

Hosted recovery preparation and final workspace collection use
`execution_agent snapshot-capture`. A repeated capture can return a small
unchanged receipt instead of sending file content through Docker and writing
another Runtime spool. Persisted snapshots remain exact `workspace.snapshot.v1`
frames; restore, API publication, object references and authorization retain
their existing contracts.

## When this saves I/O

The optimization applies only when a capture actually runs and the same runner
already holds a verified manifest for the caller's authoritative snapshot.
Existing generation/activity witnesses can already avoid capture entirely.
Those skipped safe points receive no additional benefit here. A write followed
by a revert can require capture while still matching the cached content.

The runner retains one manifest, bounded by the existing 1 MiB manifest limit.
It binds the manifest to all four descriptor fields: frame SHA-256, frame byte
size, expanded byte size and file count. A missing or mismatched baseline causes
a full capture. Creating a runner, replacing an execution or beginning streamed restore
leaves the cache empty. Restore does not seed it; the first subsequent capture
still transfers and spools a full frame. Capture failure discards the cache.
An empty workspace has no cached manifest and keeps the existing zero-byte
persisted representation.

For matching nonempty content of size B, both complete source reads remain:
one to construct the manifest and one to verify the copied content. The second
pass writes to a sink. File-body transfer through Docker and Runtime spool
writes drop from B to zero; the small request manifest and response remain.
Changed and cold captures still transfer a full frame. API upload/publication
already skipped equal descriptors, so this change does not claim a new saving
there or a reduction in source reads. No latency claim follows from byte counts.

## Capture contract

The root helper receives bounded JSON on stdin and requires EOF:

```json
{
  "schema": "workspace.snapshot.capture.v1",
  "captureId": "capture-1",
  "baseline": {
    "snapshotSha256": "sha256:<64 lowercase hex digits>",
    "manifest": {
      "schema": "workspace.snapshot.v1",
      "files": [{
        "path": "note.txt",
        "sizeBytes": 3,
        "sha256": "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "executable": false
      }]
    }
  }
}
```

`baseline` is null for a cold capture. A supplied manifest must be nonempty and
pass the existing canonical path, ordering, overlap, size and digest checks.
`captureId` is at most 128 ASCII characters
from letters, digits, hyphen, underscore, period and colon. Unknown fields and
unsupported schemas fail.

Stdout begins with a four-byte big-endian JSON length and a response of at most
1 KiB:

```json
{
  "schema": "workspace.snapshot.capture.v1",
  "captureId": "capture-1",
  "disposition": "unchanged",
  "baselineSnapshotSha256": "sha256:<64 lowercase hex digits>",
  "manifestSha256": "sha256:<canonical manifest JSON digest>"
}
```

An unchanged receipt must match this invocation's capture ID, baseline frame
digest and canonical manifest digest, and must be followed by EOF. A `snapshot`
response has `baselineSnapshotSha256: null` and is followed by an ordinary v1
frame, or EOF for an empty workspace. Runtime independently validates every
file hash, the canonical manifest, trailing EOF and the response's manifest
digest. Only successful helper exit makes either result usable. A malformed
frame or nonzero exit cannot seed the cache, even if the helper emitted a
plausible receipt first. Helper failure retains its exit code with empty stderr.
`snapshot-collect` remains the explicit forced-full capture command.

## Consistency and authority

The existing activity exclusion remains held during capture. The helper checks
for live agent processes before scanning and after second-pass validation. It
does not terminate background processes. Protected input inventory, secure path
opening, file ownership/type validation and metadata-before/after checks remain
in force. Equal-size content replacement fails the second-pass digest check.
The receipt is emitted only after that pass and the final idle check succeed.

These checks rely on the existing excluded-dispatch and idle-sandbox boundary;
they do not promise an atomic cross-file snapshot under arbitrary external
writers. Recovery witness completion and the before-publication fence are still
required. A cached manifest is content metadata, not an activity witness or an
authorization grant. It cannot authorize an old Run, historical input or a
foreign execution. Terminal collection keeps its existing quiesce behavior.

## Validation and rollback

The helper tests cover retained second reads, equal-size mutation rejection,
idle checks, unchanged output suppression, exact changed frames, empty content
and metadata changes. Runtime tests cover descriptor binding, capture identity,
forged/oversized/truncated receipts, full-frame integrity, failed helper exits,
cache replacement and consecutive captures through one runner's activity
boundary, including restore invalidation. Existing activity exclusion, immutable
upload and publication-fence tests remain required. Linux process/file behavior
and real Docker byte measurements are separate acceptance evidence.

There is no persistent-format migration. Roll back Runtime and helper together;
the previous full-capture implementation can read all stored frames. Do not
deploy mismatched helper/Runtime command contracts or silently fall back after
a malformed response.

The helper is copied into the tool image by
`packages/hosted_execution/Dockerfile.agent`; Runtime executes that image's
`/opt/centaeris/bin/execution_agent`. It does not inject a replacement binary.
`WORKSPACE_GENERAL_IMAGE` is resolved to an immutable image ID and container
adoption checks the expected image. Changing a tag does not update an existing
instance. Coordinate Runtime and tool-image publication, and drain existing
executions with the matching old Runtime before switching workers. A durable
waiting or otherwise non-drainable execution requires explicit handling under
the existing recovery/terminal policy; this change does not authorize killing
its processes or substituting a new image behind its execution identity. Do not
let the new Runtime adopt old-helper instances. Compatible v1 snapshot bytes
alone do not bypass the normal execution/image binding checks on restore.
