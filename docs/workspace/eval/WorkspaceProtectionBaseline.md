# Workspace protection measurements

This experiment measures the current file-snapshot path before changing capture
frequency, process lifecycle, storage format or retention. File-only snapshots do
not establish process recovery. Run the durable execution probe separately.

## Isolation and evidence

Use `perf/harness/control.py` for deployment experiments. Its preflight checks the
isolated project, exclusive volumes, loopback ports, image identities and resource
samples. Never point it at an application deployment. Use unique evidence and
workload directories; preparation refuses existing directories and aliases.
Record the exact source revision and changed-file hashes, image digests, host,
resource limits and explicit cache conditions. No body, prompt, credential or
arbitrary error text belongs in measurement events.

`perf/harness/workspace_protection.py prepare --root <new-path>` writes synthetic
256 MiB cold data, a 4 KiB hot file and an append log. Every scenario starts from
the same manifest and tree hash. Reduce `--cold-bytes` only for smoke tests and
report the changed size. The controlled patterns are deliberately compressible;
they cannot alone establish the efficiency of an archive or deduplication tool.

Run `mutate --root <path> --workload <name>` between actual product requests:

| Workload | Change |
|---|---|
| `unchanged` | No bytes change |
| `hot-file` | Replace the 4 KiB hot file |
| `large-overwrite` | Replace 4 KiB inside the large cold file |
| `append-log` | Append bounded data to the log |
| `rename-delete-revert` | Rename/delete/recreate to identical final bytes |
| `background-write` | Prepare a log modification; separately run the bounded writer below |

For concurrent capture run `background-write --root <path> --seconds 10` as a
separate process while exercising the existing product capture operation. Record
whether capture was accepted, deferred or rejected and retain the failure result.
A mutation performed before capture alone is not a concurrent-write experiment.
Keep the existing capture exclusion and quiesce checks. The fixture generator
must finish and close files before its initial manifest is accepted.

## Accounting contract

Protection events retain `schema: workspace.perf.v1`, with
`phase: workspaceProtection`, a bounded `source`, `operationId`, `metric` and
numeric `value`. Optional Session/Run/environment identities and trigger names
contain identifiers only. Operation IDs identify measurement attempts, not
authority to execute or read user data.

Byte/count metrics are deltas for observed work, including failed attempts.
`temporaryBytes` and `retainedBytes` are occupancy samples scoped to the source
and operation: preserve last/peak values rather than adding repeated samples.
Stage timings remain separate; nested timers must never be summed into a
manufactured end-to-end latency. Logical bytes, user-space I/O and physical disk
I/O are distinct: no counter here automatically proves filesystem allocation,
storage-device writes or freed blocks.

API hardlink publication increments `publishedLinks`; it does not add a second
full copy to `apiWrittenBytes`. A repeated upload may still receive and hash
bytes and verify the existing object. Include those operations even when the
final result is idempotent. Actual transfer-read counts describe application
payload consumed by HTTP, not Ethernet/TLS bytes or proof of delivery.
`apiReceivedBytes` includes the four-byte request frame, metadata and snapshot;
`apiWrittenBytes` samples the closed temporary file's actual logical length.
`snapshotLogicalBytes` is the sum of captured file contents, excluding the frame
and manifest. Source reads include both hashing and copying, including partial
work before failure. GC counts last-link logical bytes only for immutable final
snapshot files; open descriptors can retain physical allocation after unlink.
Racing temporary upload files are unmeasured because neither final size nor
deletion attribution can be established by a path sample.

Restore download timing measures actual response-body reads; connection setup
and response headers are outside that timer. Restore application timing covers
the whole helper operation and can overlap those reads. Terminal upload timing
ends when HTTP returns a response: its `ok` means a transport response arrived,
not that the server committed the snapshot. Recovery staging timing also covers
response validation. Use the API commit result to establish publication.

Collect API, Runtime, worker and GC logs with the existing
`perf/harness/collect_observations.py` time window and manifest. Missing services,
empty logs and malformed protection samples fail collection. Then filter the
protection records and use:

```sh
python perf/harness/workspace_protection.py summarize \
  --events protection-observations.jsonl --output new-summary-directory
```

The summary writes `summary.json`, `io-and-transfer.csv` and
`restore-phases.csv`. Missing counters are `null` / `not-measured`, never zero.
Counter totals contain observed deltas only. Per-operation coverage lists every
source operation with a missing sample, and CSV marks those sums `partial`.
An unrelated successful sample cannot prove that an omitted failed attempt did
zero work. Coverage is conservative: some stages do not perform a metric at all;
resolve this from the raw attempt and trigger, never silently fill a missing zero.
Keep the raw event files, source/host manifest, initial/final data manifests and
each request outcome beside these reports. Cold and warm cache results are
separate runs; do not globally drop a shared host's page cache.

## Acceptance

For each scenario reconcile captured files, snapshot logical bytes, actual read
and write deltas, upload attempts, commit outcome and restore hash. Include at
least one failed/partial attempt and one repeated upload. Confirm occupied and
retained space independently; omitted samples leave the corresponding conclusion
open. A passing unit test of accounting does not establish deployment cost.

This branch adds observations and an experiment harness only. It must not change
the terminal process cleanup, capture cadence, input authorization, current
checkpoint selection, or garbage-collection ownership rules. Snapshot reuse and
incremental archives require a separately reviewed change backed by this data.

## Direct helper baseline

`perf/harness/workspace_snapshot_probe.py` runs the real instrumented
`execution_agent` in isolated runsc containers, validates the complete snapshot
frame and restores it into another owned container. It compares each restored
file with the selected frame. Use an existing dedicated loop filesystem of at
most 8 GiB as the evidence parent:

```sh
python3 perf/harness/workspace_snapshot_probe.py \
  --evidence-parent <owned-loop-mount-root> --experiment-id <unique-id> \
  --image-id sha256:<exact-image-id> --agent <exact-execution-agent> \
  --cold-bytes 268435456
```

Each sandbox has one CPU, 1 GiB memory, 128 host PIDs, a read-only root filesystem,
no network, and bounded data/control tmpfs mounts. The harness checks actual
kernel cgroup limits. Only the exact helper and synthetic fixture module are
mounted read-only. The container lifetime and individual commands are bounded.
Cleanup requires the exact container identity and ownership label.

The `full-a1726e90` experiment used 256 MiB of cold data, a 4 KiB hot file and a
nine-byte log: 268,439,561 source bytes in three files. All six initial manifests
matched. Every baseline capture scanned four entries, read 536,879,122 bytes
and produced a 268,440,053-byte frame. The measurements after mutation were:

| Workload | Actual source bytes read | Output frame bytes | Capture wall time | Restore wall time |
|---|---:|---:|---:|---:|
| Unchanged | 536,879,122 | 268,440,053 | 6,803.5 ms | 6,406.3 ms |
| 4 KiB hot-file replacement | 536,879,122 | 268,440,053 | 6,736.7 ms | 6,640.2 ms |
| 4 KiB overwrite inside the large file | 536,879,122 | 268,440,053 | 6,619.3 ms | 6,920.6 ms |
| Append 3,584 bytes | 536,886,290 | 268,443,640 | 6,963.7 ms | 6,893.0 ms |
| Rename, delete and recreate identical content | 536,879,122 | 268,440,053 | 6,194.0 ms | 6,966.3 ms |
| Concurrent background writes | 268,439,561 | 492 partial bytes | 2,630.0 ms | Not attempted |

The background capture failed with source-changed evidence. Its partial frame
was never offered for restoration. All other restorations matched the selected
bytes; unchanged and recreated-content frames matched their baselines exactly.

These are single paired observations with caches warmed by preparation and
verification. Capture/restore wall time includes Docker exec and harness file
I/O; it is not a Runtime timer or a latency percentile. The actual helper source
reads show two passes over the complete successful input. For a 4 KiB mutation,
the output frame is about 65,537 times the changed bytes. This ratio describes
frame output, not block-device writes, WAL, network billing or end-to-end cost.
For unchanged input, report absolute work; a ratio to zero changed bytes is
undefined.

The raw evidence binds the executable, image, harness and fixture hashes. This
full-size run preceded the container-lifetime guard; its original harness is
retained alongside the evidence. The final bounded harness is separately checked
with the 8 KiB smoke workload. No full-size rerun is implied by that validation.

Automatic Runtime triggers, activity-based reuse, HTTP transfer, API publication,
GC and cold-cache behavior remain unmeasured in this helper experiment. Product
instrumentation and tests cover those accounting boundaries, but they are not
deployment-cost observations. Runtime events carry the existing Run/Job identity
and fixed trigger where available; a stable Environment identity does not yet
exist and is not fabricated for the experiment.

## Decisions supported by this evidence

The complete-frame path has measurable work even when its final bytes are
unchanged. The next optimization candidate is reuse of an already sealed snapshot
when activity/generation evidence, authorization, leases and retention all permit
it. Measure the actual Runtime trigger path before estimating how frequently
that cost occurs or claiming a saving.

The safe rejection of an active writer supports retaining capture exclusion and
quiescence checks. Persistent background tasks require a separately verified
process-and-file capture protocol. Simply removing the active-process guard
would not produce a consistent recovery point.

Stable environments and cheaper archive encodings address separate costs.
Session-to-environment continuity can reduce unnecessary reconstruction and keep
registered tasks alive; incremental archives can reduce transferred or newly
stored content. Compare archive candidates using the same consistency and
retention requirements before selecting one. The small native process probe and
this file-only benchmark use different workloads and must not be ranked by their
elapsed times.
