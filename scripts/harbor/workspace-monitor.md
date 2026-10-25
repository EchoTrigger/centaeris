# Workspace benchmark monitor

The monitor shows the controller's public projection. It does not start a trial,
submit a message, run a verifier, read Workspace databases, or cancel work. The
English interface uses a black background and square corners. A new empty
projection directory displays **No live run**; starting the monitor does not start
the paid pilot.

## Start on the Linux test host

Use a dedicated absolute directory containing sanitized `manifest.json`,
`events.jsonl`, optional `output.json`, and the controller's empty writer lock. Keep provider configuration, credentials, full task output, and full
job artifacts outside this directory. The controller owns these files and may
replace them atomically; the monitor only reads them. Its numeric user
`65534:65534` needs directory traversal and file read permission. For this public
projection only, directory mode `0755` and data-file mode `0644` are sufficient;
the writer lock may remain `0600`. The writer must preserve reader permissions
when creating or atomically replacing files. Do not change permissions on the
private job directory or credentials to make this mount readable.

From the repository root, with the existing directory selected explicitly:

```sh
export CENTAERIS_BENCHMARK_PROJECTION_DIR=/absolute/path/to/public-projection
docker compose -p centaeris-benchmark-monitor \
  -f scripts/harbor/workspace_monitor.compose.yml config
docker compose -p centaeris-benchmark-monitor \
  -f scripts/harbor/workspace_monitor.compose.yml up -d --build
```

The required directory variable has no default. Compose refuses a missing source
rather than creating a directory at a mistyped location. Inspect the rendered
configuration before starting. Confirm that port 9000 is free first; do not stop
an unrelated service to make it available.

The image contains one Python module and three static files. Its build context
uses an explicit whitelist. The base tag is
`python:3.12.10-slim-bookworm`; record its resolved digest, the final built image ID,
and the exact source commit with deployment evidence. A tag alone is not an
immutable image identity. Building downloads the base image, but the monitor has
no provider dependency and makes no model calls.

## Open from Windows

The Python listener is fixed to the VM's `127.0.0.1:9000`. Linux host networking
allows the container to use that guest loopback address without a published LAN
port. No Docker `ports` mapping is used.

Create an SSH tunnel from PowerShell, using the existing local SSH key:

```powershell
ssh -N -T -i 'C:/Users/Luv Letter/.ssh/centaeris-vm' `
  -o ExitOnForwardFailure=yes `
  -L 127.0.0.1:9000:127.0.0.1:9000 root@192.168.37.129
```

Then open **http://127.0.0.1:9000**. The SSH process must remain connected while
viewing. Closing the browser or tunnel leaves both the monitor and trials
running. If local port 9000 is occupied, use local port 9001 in the tunnel and
browser; the VM service remains on 9000.

## Restart and inspect

```sh
docker compose -p centaeris-benchmark-monitor \
  -f scripts/harbor/workspace_monitor.compose.yml ps
docker compose -p centaeris-benchmark-monitor \
  -f scripts/harbor/workspace_monitor.compose.yml logs --tail=50
docker compose -p centaeris-benchmark-monitor \
  -f scripts/harbor/workspace_monitor.compose.yml restart workspace-benchmark-monitor
```

Set the same projection variable for these commands. Restarting this service
rebuilds its view from the projection files. It does not stop or resume trials.
The restart policy is `unless-stopped`: Docker restarts an exited service and
starts it after daemon reboot unless it was deliberately stopped. An unhealthy
healthcheck alone does not trigger a Docker restart.

## Access and resource limits

| Boundary | Configuration |
|---|---|
| Listener | Guest loopback only; remote access through SSH |
| Container identity | Numeric user and group 65534 |
| Filesystem | Read-only root and one read-only `/job` mount |
| Capabilities | All dropped; no new privileges |
| Process budget | 32 PIDs |
| Memory / CPU | 128 MiB / 0.25 CPU |
| Logs | Two rotated files of at most 5 MiB each |
| Health | Local HTTP response every 30 seconds; 3-second check deadline |

Only the dedicated public projection directory is mounted. The container receives
no Docker socket, provider key, maintenance token, database connection, or whole
job directory. Existing files or logs outside that directory are not inspected.
Host networking provides network reachability; loopback binding limits incoming
exposure but is not an outbound firewall or isolation from other guest processes.
The page has no application login, so other users or processes on that host can
read it. Keep the VM and SSH account access controlled.

The healthcheck measures server availability, not trial success or controller
liveness. The page separately shows missing, invalid, waiting, or stale
projection data. A stopped controller can leave the last observation visible;
its age is the indication to inspect the controller. The monitor is not an
execution authority or an automatic recovery service.

The server reads bounded event chunks and retains a bounded recent log window;
it is intended for one local viewer or a few viewers. PID, memory, and CPU limits
can make it unavailable under load without changing trial execution. Projection
writers must bound and rotate their own files and avoid credentials in all
exported fields. Redaction is a secondary precaution, not permission to place
private material in the mounted directory.

Transient filesystem read failures are retried on the next page refresh from the
last validated event cursor. Previously confirmed lifecycle facts remain visible;
output hydration waits until lifecycle observations are readable again. Corrupt
events, sequence gaps and mismatched output identities remain explicit errors.

## Optional rolling output

Each slot has an independent, keyboard-scrollable output panel. Scrolling away
from the bottom pauses following; **Follow output** returns to the latest output.
Refreshes retain scroll and focus. A slot keeps its last observed trial after
resource release, while the ownership label continues to use controller facts.
Untrusted output is rendered as text. Earlier output may be outside this bounded
view; complete trial artifacts remain separate.

Elapsed time runs from the first observed `running` trial event to the first
observed terminal outcome. It is not model latency or cleanup duration. Reloading
rebuilds these anchors from events. Missing anchors display **Unknown**; stale or
disconnected observations freeze the running display. Monospace, tabular figures
and a fixed nine-character box keep the clock from moving adjacent content.
Only slot elapsed clocks are displayed; the page header has no observation-age
timer. Stale and disconnected observation states remain textual status indicators.

`workspace_output.py` is a separate optional observer, never part of the execution
driver or the 9000 container. It logs in with the isolated benchmark account and
reads only the public SSE endpoint for an explicitly projected Session/Run pair.
It has no provider key, maintenance authority, Docker socket or database access.
Its request allowlist permits CSRF/login and that exact run's GET event stream;
all execution requests are rejected. It does not submit messages or grade work.

Run it outside the monitor container using Python 3.12 or later:

```sh
python3 -B /absolute/source/scripts/harbor/workspace_output.py \
  --job-dir /absolute/path/to/public-projection \
  --seed-path /absolute/private/benchmark-seed.json \
  --api-origin http://127.0.0.1:18280
```

The seed is the existing isolated account seed, a regular file of at most
64 KiB, mode `0600`, outside the public projection. Credentials stay out of command
arguments, stdout, the public directory and task environments. Host operators can
supervise this command with an independent systemd unit (`Restart=on-failure`,
`NoNewPrivileges=true`, read-only system/source and write permission only to the
selected projection). Rebind both observers explicitly when starting a new
batch; changing the viewer does not change the candidate or resume a batch.

The producer atomically replaces one `output.json`, with the job identity,
observation timestamp and at most two trial windows. Each window binds
`trialId`, `sessionId`, `agentRunId` and the committed `sourceSequence`. Retention
is at most 40 entries and 64 KiB UTF-8 text per window, at most 8 KiB per entry.
Raw SSE events are limited to 256 KiB; larger events make output unavailable
without changing trial execution. There are at most two concurrent GET streams,
each with a three-second read slice, followed by a two-second observation pause.
No unbounded transcript, ID set, background queue or sample series is retained.

Live text is a revision snapshot, not an incremental token: newer revisions
replace the overlay. Committed assistant messages and phase barriers retire the
overlay. Only committed events advance reconnection cursors. Assistant text,
normalized tool arguments and model-visible results are selected; reasoning,
request bodies and arbitrary provider fields are not exported. Unavailable
bodies remain explicitly unavailable. Redaction is a secondary precaution;
operators must still keep credentials out of benchmark task content.

The viewer reads only the fixed file, rejects symlinks, unknown schemas, oversized
files and mismatched job/Session/Run bindings, and never changes job outcomes on
observer failure. Missing identities show **Output not observed**. The latest
observation can remain visible with an explicit stale/unavailable label. Closing
the page, observer failure or stopping the observer cannot cancel a trial.

Focused tests include `test_workspace_output.py` (real local SSE login/reconnect
and request scope) and `test_workspace_monitor.py`. Set
`WORKSPACE_MONITOR_BROWSER=1`, `NODE_BINARY`, `PLAYWRIGHT_MODULE` and
`CHROME_BINARY` to enable real-browser geometry, scroll/focus, disconnect,
terminal-time, reload and mobile checks. They use isolated test data and no model.


## Observation health and results

The controller's projection thread emits a heartbeat every five seconds,
independently of model output and lifecycle events. Thirty seconds without a
heartbeat means observation is interrupted, not that a task failed. The first
observation allows a thirty-second startup grace period. Lifecycle updates do
not extend the heartbeat deadline. The heartbeat only confirms the observation
thread can write; it does not prove the agent or verifier is advancing.

A committed assistant message renders Core's `modelMarkdown` field. Neither a
reply nor missing reply text establishes completion: only terminal Run events
do so. The page separately displays Agent status, Verifier status, reward and
cleanup. Reward 1 is Passed, reward 0 is Not passed, and missing reward has no
inferred score. Fully terminal jobs with confirmed cleanup retain their final
output and frozen elapsed time without requiring further heartbeats.
