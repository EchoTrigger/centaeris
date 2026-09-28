# Local OpenSSH and schedules

These are local Host/application features shared by Desktop and TUI. Core's
runtime contracts, turn loop and execution semantics are unchanged. Hosted
Workspace is outside this implementation.

## OpenSSH

Use the installed `ssh` executable and the user's OpenSSH configuration,
credentials agent and known-host policy. No credentials are copied into Centaeris.

```text
centa ssh connect dev
centa ssh exec dev "uname -a"
```

`connect` inherits the native terminal, before TUI raw mode starts. OpenSSH handles
interactive login and host-key prompts. It does not open an embedded terminal.
`exec` creates a local Session in the current directory and starts a managed
background SSH process, printing its Session and process identities as JSON.
Open that Session in Desktop Task or TUI to inspect output and stop the process.

Inside an existing TUI Session:

```text
/ssh dev uname -a
/process list
/process output <processSessionId>
/process stop <processSessionId>
```

Desktop and TUI Agents receive `ssh_start` with `destination`, `command` and
optional `timeout_ms` (zero/omitted means no deadline) and optional
`connect_timeout_seconds`; completion and output use the existing `process_list`,
`process_read` and `process_stop` tools. Agent starts receive automatic completion
notifications; direct user CLI/TUI starts remain user-managed processes.

The Host method `ssh_start` takes `sessionId`, `serviceInstanceId`, `operationId`,
`destination`, `command`, and `timeoutMs` inside the usual `request` envelope.
Its result is an ordinary process snapshot. Repeating the same request in the
same service instance is idempotent. A different service instance is not allowed
to adopt or replay the old process start.

Managed execution uses `-T -o BatchMode=yes --`, followed by
the destination and one remote command string. The local Host does not interpret
that string; the remote shell does. Destinations cannot inject SSH options.
Interactive authentication is intentionally unavailable in managed execution.
OpenSSH configuration controls the default connection timeout. Only an explicit
`connectTimeoutSeconds` Host argument (`connect_timeout_seconds` tool argument)
adds `-o ConnectTimeout=N`. This is separate from the optional whole-command
`timeoutMs` / `timeout_ms`; explicit deadlines are no longer capped at 24 hours.
Stopping local SSH, timeout, disconnection or exit 255 is not proof that a remote
command stopped or had no effects. Do not blindly repeat a command after an
unknown outcome. This is not a remote Runtime, remote workspace mount, SFTP
browser or reconnectable remote process service.

## Persistent local schedules

The primary creation and management surface is conversation with the Agent in
Desktop or TUI. For example: "Every day at 09:00 UTC, review this workspace using
the current model and tell me what needs attention." The Agent creates the plan;
the user does not need to write a specification file or enter CLI commands.

The Agent calls `schedule_manage` with `action: "context"` to obtain the bound
workspace, current configured model and explicit effort, clock and service
status. This response contains no credentials. It then creates the plan, enables
the local scheduler when needed for the user's request, and confirms the actual
next time and time zone from successful tool results. Ambiguous timing must be
clarified rather than guessed. Service enablement affects all enabled plans;
the Agent should inspect existing plans when the service was stopped.

Listing, changing, pausing and deleting plans are also conversational actions.
The CLI below is a development/diagnostic surface, not a required user workflow.

Plans, immutable occurrence specifications and history are stored atomically in
the active profile's `runtime/schedules.sqlite3` (private schema version 1).
The previous `schedules.json` is imported once in a transaction, with an exact
`json.pre-sqlite.backup` and the original file preserved. No model secret
is stored there. It is a Host-owned store, not an automatically executed project
file. Unknown schema versions and fields fail loudly.

```text
centa schedule create "C:/work/daily-review.json" review-plan-1
centa schedule list [cursor]
centa schedule service start
centa schedule history <scheduleId> [cursor]
centa schedule run <scheduleId> manual-review-1
centa schedule pause <scheduleId>
centa schedule resume <scheduleId>
centa schedule update <scheduleId> <revision> "C:/work/daily-review.json"
centa schedule delete <scheduleId>
centa schedule service stop
```

In the TUI, replace `centa schedule` with `/schedule`; quote file paths containing
spaces. Results use a scrollable panel. `operationId` arguments for create and
manual run are caller-selected stable identities: repeat the same identity only
to reconcile that same operation. Updates require the revision returned by list
or create. A conflict requires reading the plan again.

A UTF-8 specification file has this shape (substitute a configured provider,
model, supported effort and existing absolute directory):

```json
{
  "name": "daily-review",
  "cwd": "C:/work/project",
  "prompt": "Review local changes and report findings. Do not edit files.",
  "cron": "0 9 * * *",
  "at": null,
  "timezone": "Asia/Taipei",
  "model": {
    "providerId": "your-provider",
    "model": "your-model",
    "thinkingMode": "high"
  }
}
```

`thinkingMode` must be an explicit supported mode; `null` is accepted only for a
model without reasoning modes. The scheduled admission uses this selection
without changing global model settings. Provider credentials and ordinary Host
execution policy are resolved normally at execution time; scheduling grants no
additional permissions. Changing the plan affects future occurrences only.

For a one-shot, set `cron` to null and `at` to a future Unix timestamp in
milliseconds. Exactly one must be non-null. Cron has five fields, POSIX weekday
numbering, and an explicit IANA time zone. Presets can use `0 * * * *` (hourly),
`0 9 * * *` (daily), or `0 9 * * 1` (Monday). Cron parsing and time-zone behavior
come from `croner` and `chrono-tz`; the Host does not implement another parser.

Desktop and TUI Agents use `schedule_manage` with an `action`:
`context`, `list`, `create`, `update`, `pause`, `resume`, `delete`, `history`, `run`, or
`service`. Tool parameters are `operation_id`, `schedule_id`,
`expected_revision`, and `spec.model.provider_id` / `thinking_mode`.
The corresponding Host method is also `schedule_manage`, with camelCase fields.
`context` is Agent-only and bound to its Session; it is not a Host method action.
Desktop currently uses these Agent tools and the Host bridge; this slice adds
no separate scheduling form or remote-connection screen.

## Execution and recovery

- Scheduling starts disabled. `service start` enables persistent background
  scheduling. Enabled plans keep the shared Runtime alive after client windows
  close. `service stop` disables automatic future triggers, without cancelling
  admitted work. Manual `run` works even with automatic scheduling off.
- The machine must be awake and the Runtime must be running. There is no OS
  startup registration, wake timer or cloud execution. After a full service exit
  or reboot, starting a Centaeris client starts the Runtime again; the saved
  service setting is retained.
- Each occurrence creates a fresh Session. Plan ID, revision and scheduled time
  identify automatic occurrences; manual runs use their operation identity.
  Intent and next-fire advancement commit before Session/Run admission. Recovery
  reconciles the same deterministic identities instead of creating replacements.
- Missed recurring occurrences coalesce into the latest due occurrence, which
  runs once; the clock advances directly to the next future time. A one-shot
  runs once when the service resumes unless its explicit optional `expiresAt`
  (`expires_at` in tool arguments) has passed, producing `expired`. There is no
  implicit 60-second expiry. A still-active previous occurrence produces
  `skippedOverlap`; no waiting backlog is accumulated.
- History distinguishes `pending`, `running`, Agent terminal states,
  `failedAdmission`, skipped occurrences and `sessionDeleted`. Admission
  uncertainty retains its original identity and error for reconciliation.
  Deleting an admitted Session must not recreate it.
- Pausing or deleting a plan affects future triggers. Existing occurrence
  history remains; use the ordinary Agent cancellation interface to stop a run.
- Historical counts do not disable schedules or reject admission. Workers query
  indexed due plans and active occurrences only. Completed history and deleted
  plan identities stay on disk for history and deduplication. `list` and `history`
  return up to 50 rows by default (maximum 100), with `nextCursor`; pass that as
  `cursor` to continue in the same scope. History is newest first. Desktop offers
  Load more; Agent and diagnostic CLI callers can request subsequent pages.

## Validation

Run the normal Rust/Node source gates and:

```text
cargo build --locked -p centaeris-runtime --bin centaeris-runtime -p centaeris-tui --bin centa
node scripts/test-host-automation.mjs
node scripts/test-agent-schedules.mjs
node scripts/test-process-agent.mjs
node scripts/test-process-sessions.mjs
```

Automation acceptance uses an isolated profile, a loopback model server and an
SSH executable fixture. It verifies argument forwarding and process integration,
not authentication against a real remote SSH server. Manual real-host acceptance
should cover a configured alias, native authentication, remote failure and
disconnection without automatic command replay.
