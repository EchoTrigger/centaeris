# Native runsc durable execution probe

This bounded correctness experiment runs a real process tree using native runsc,
captures it, confirms the source sandbox has stopped and its physical process
has disappeared, and restores into a different sandbox identity. The oracle is
outside the sandbox. A fresh process started from saved files cannot satisfy its
memory challenges. This is an isolated research harness, not a product lifecycle
change or a production recovery promise.

## Host and isolation contract

Use an explicitly authorized Linux test host with cgroup v2 CPU, memory and PID
controllers, root access, runsc, static GCC/pthread libraries, `fallocate`,
`mkfs.ext4`, `mount`, `umount`, `losetup`, `findmnt` and `lscpu`. The evidence
parent must already exist and must not traverse symlinks. Every experiment ID
creates a new directory and is never reused.

The harness creates one 256 MiB preallocated file and mounts its ext4 filesystem.
It verifies the mount and exact loop backing file before launching runsc. The
root filesystem contains only the compiled static probe, has a recorded digest
and is read-only. There are no host bind mounts, Docker calls, application
accounts, databases, volumes, credentials or model requests. The workspace is
a 16 MiB memory-backed tmpfs, with a 16 MiB file-size limit. UID/GID are 1000,
capabilities are empty, and networking is disabled.

The OCI spec sets one CPU, 512 MiB memory and 96 host PIDs. Kernel `cpu.max`,
`memory.max` and `pids.max` are checked after both start and restore; writing a
spec alone is not accepted as enforcement. Individual commands have deadlines
and the experiment has a 240-second deadline. Evidence and the disk image remain
after cleanup. Cleanup deletes only the two precise owned sandbox identities,
unmounts runsc's exact owned null network namespace and then the exact loop
mount. Cleanup failure fails the overall outcome and remains visible.

```sh
python3 perf/harness/durable_execution_probe.py \
  --evidence-parent /data \
  --experiment-id unique-smoke-id \
  --source-revision "$(git rev-parse HEAD)"
```

The revision is the checkout baseline; the manifest also records the exact
harness, C source and compiled binary hashes, so uncommitted candidate source
cannot be confused with that commit. No image is pulled: `rootfsIdentity` names
the single static binary filesystem. Keep raw evidence outside the source tree.

## What the oracle verifies

The C workload generates a random nonce and a separate anonymous mmap challenge
in memory. It never writes either to a workload file. The parent retains a live
child process, a pipe with unread bytes, an open ordinary file at a nonzero
cursor, an unlinked-but-open file at a nonzero cursor, a hard link, and a renamed
file with specific permissions. A thread advances a counter, writes an append
log and updates a shared file mapping.

Before capture, an external control request observes real background progress,
then requests an application-level freeze. The freeze acknowledges only after
the writer holds its mutex and stops mutations. A separate client invocation
reconnects to the same live control socket. One native `runsc checkpoint`
captures process state with the memory-backed tmpfs. There is no separate,
potentially inconsistent filesystem checkpoint and no `--leave-running`.

After source stop and deletion, a new sandbox restores. All memory, cursor,
counter, pipe-length, child-ID and mmap values must equal the external oracle.
Actual reads then verify the child can consume the original pipe bytes and both
open descriptors still access their expected contents. The writer is resumed
and must continue advancing coherent mapped state. This proves the tested
application-quiesced layout; unquiesced arbitrary applications are untested.

The checkpoint seal covers every file, size, digest and runsc binary identity.
An actual truncated copy, a wrong expected digest and a mismatched runtime
identity must fail admission before invoking restore. These are **harness
admission** checks; they do not certify runsc's handling of unvalidated images.

## Evidence and interpretation

The harness writes:

- `source-and-host-manifest.json`: exact source/runtime/rootfs identities,
  kernel, CPU, architecture, OCI spec, layout and limits.
- `oracle-*.json`, source state and restored descriptor verification: raw
  correctness evidence outside the failure domain.
- `capability-matrix.json`: `supported`, `unsupported`, `failed` or `not-tested`
  for each distinct capability; a missing observation is never zero or success.
- `fault-results.jsonl`: rejected corruption/identity cases and their layer.
- `io-and-transfer.csv`: retained logical and allocated checkpoint byte gauges;
  cross-host transfer is explicitly unmeasured. These are not actual I/O counters.
- `restore-phases.csv`: non-overlapping start, checkpoint and restore timings.
- `commands.json`, process logs and `outcome.json`: exact commands, failures,
  overall status and cleanup results. No failed attempt is erased.

Review these alongside the separate workspace protection measurements. A few
smoke runs cannot establish tail latency, failure probability, host capacity,
cost savings or independent failure-domain protection. Management reconnect,
process recovery, filesystem recovery and external connection recovery are
distinct capabilities. Cross-host restore, unquiesced capture, external network
connections, disk-backed workspaces and the product Docker path stay
`not-tested` until their own evidence exists.

## Observed layout decision

The initial experiment against runsc `release-20261005.0` rejected
`checkpoint --fs-checkpoint-paths=all-tmpfs` for this memory-backed layout with
`no checkpointable filesystems`. That split-filesystem option applies to
checkpointable disk-backed tmpfs, not all possible tmpfs mounts. Its failure
must not be hidden by claiming that filesystem-only capture passed.

The explicit memory-backed layout above subsequently restored the real process
and file state using one ordinary checkpoint. It is the chosen correctness
smoke layout. This is not a decision to adopt memory-backed workspaces or this
256 MiB test disk in production. No fallback to runc or read-file-and-restart is
implemented. Firecracker remains unevaluated rather than implicitly selected.

References: [native checkpoint/restore](https://gvisor.dev/docs/user_guide/checkpoint_restore/)
and [filesystem snapshot scope](https://github.com/google/gvisor/blob/master/g3doc/user_guide/fs_snapshot.md).
The installed binary help and recorded command results govern the actual run.

## Regression tests

Two successful single-host smoke executions with the recorded
`release-20261005.0` binary produced the following raw measurements. Both restored
the in-memory challenges, process tree, file descriptors and quiesced writer,
and completed owned-resource cleanup:

| Experiment | Checkpoint elapsed | Restore elapsed | Retained logical bytes |
|---|---:|---:|---:|
| `smoke-final-051f74a3` | 371.7 ms | 675.1 ms | 389,489 |
| `mount-guard-71f32a0b` | 309.4 ms | 675.5 ms | 389,396 |

These are two tiny-workload correctness observations, with uncontrolled cache
state. They do not establish a latency percentile, a failure rate, a production
memory budget or a comparison with the 256 MiB file-only workload. The second
run also uses the cleanup guard that verifies the loop backing file again before
deleting owned sandboxes or unmounting. Raw manifests bind each observation to
its exact executable and harness hashes.

The supported result justifies further native runsc layout and compatibility
experiments. It does not justify switching the product to memory-backed storage,
claiming cross-host recovery, or terminating persistent tasks at a Run boundary.

```sh
python -m unittest perf.tests.test_durable_execution_probe -v
```

The tests first failed before implementation, and a separate cleanup regression
failed before cleanup failure affected the outcome. They cover exclusive paths,
strict resource specs, exact oracle comparison, truncation/runtime/extra-file
rejection and failure reporting. These unit tests never count as native process
recovery evidence; execute the real harness separately for that claim.
