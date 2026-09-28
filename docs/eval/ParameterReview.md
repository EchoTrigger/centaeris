# Pre-publication parameter review

Status: the maintainer approved the revised policy below. The original review
tables are retained as the baseline findings, not the current implementation.
Scope: unpublished changes since `36ff114` in the main Centaeris repository.
Existing unchanged limits, test fixtures, schema versions and ordinary CSS
dimensions are not treated as newly introduced product limits.

## Approved policy and implementation

- No cumulative admission ceilings for schedule plans/history, process completion
  records or start receipts. Indexed SQLite storage keeps historical records off
  worker paths while preserving operation identities.
- Recurring missed times coalesce to the latest occurrence; one-shots catch up
  once unless the Agent explicitly supplied expiry. Active prior work skips overlap.
- OpenSSH configuration owns default connection timeout. An explicit Agent/Host
  connection timeout is separate from the optional whole-command deadline.
- 32 concurrent background processes; 8 concurrent terminals by default, explicitly
  configurable. Completed in-memory entries retain the existing 64-entry budget.
- Keep 1 MiB output tails, 2000 terminal scrollback lines, 64 KiB read pages,
  50/default and 100/maximum catalog pages, 8 Session / estimated 64 MiB cache,
  and visible-only 5-second catalog polling.
- No 24-hour ceiling on explicit process deadlines. Omitted/zero means unlimited.
- Large terminal paste drains serially in bounded pieces with Host backpressure;
  a transport failure never automatically resends input.
- Schedule JSON and completion JSON have explicit one-time transactional imports;
  originals remain as backups. New databases are Host-private and reject unknown
  schema versions. Core contracts and hosted Workspace are unchanged.

## Original review findings

## Behavior limits requiring decisions

| Area / source | Current value | Observable consequence | Recommendation |
| --- | --- | --- | --- |
| `runtime/src/schedules.rs` | 256 plans, 4096 occurrence records | Deleted plans remain in the count. When history fills, manual runs fail and due plans are disabled. | Block publication of this behavior; separate active-plan admission from history retention, with durable deduplication preserved. |
| `runtime/src/process_agent.rs` | 256 durable completion records per profile | New agent background tasks fail until finished Sessions are deleted. | Block publication of this behavior; retire delivered records without losing retry safety. |
| `runtime/src/schedules.rs` | 60 seconds misfire grace | A task at least 60 seconds late is skipped, including a one-shot task; no accumulated backlog is replayed. | Explicitly choose missed-run semantics before publication. |
| `runtime/src/process_sessions.rs`, `terminal_sessions.rs` | 32 active, 64 retained entries, 4096 start receipts for each manager | New work can be refused; completed entries are evicted, but receipts do not roll with entries. Long-lived services eventually reach the receipt ceiling. | Keep bounded concurrent resources, redesign cumulative receipt limits with explicit retry lifetime instead of merely raising them. |
| `runtime/src/openssh.rs` | `ConnectTimeout=15` | Forces a 15-second connection timeout over user SSH config. | Prefer existing OpenSSH configuration unless the caller explicitly requests a deadline. |
| `runtime/src/process_sessions.rs` and agent tool schemas | `timeout_ms` maximum 86400000; zero permits no deadline | Explicit nonzero deadlines cannot exceed 24 hours. | Clarify whether a maximum explicit deadline is needed; it is not a platform constraint. |

## Bounded output and inputs

| Area | Current values | Effect / decision |
| --- | --- | --- |
| Process and PTY logs | 1 MiB and at most 4096 chunks; chunks up to 4096 bytes | Old output is dropped, with cursor-gap reporting. This is retained tail output, not a full durable transcript. Confirm retention policy. |
| Process reads | 64 KiB per read; wait up to 30 seconds | Bounds one response and long polling; does not impose a process lifetime. |
| PTY reads / input | Up to 16 chunks per read; encoded input limit 24 KiB; frontend sends 8192-byte pieces and caps queued input at 65536 bytes | Large pastes may be rejected by the frontend queue limit. Confirm the intended paste/backpressure behavior. |
| Desktop output | Task text keeps 256 Ki UTF-16 code units; terminal scrollback 2000 lines | Client retention is smaller/different from the Host byte ring; visible output can disappear earlier. |
| PTY size | At most 500 columns / 300 rows | Frontend clamps and Host rejects larger sizes. These are implementation choices, not PTY requirements. |
| Process argv | Program 4096 bytes; at most 1024 arguments, aggregate 64 KiB; operation ID 128 bytes | Validation boundaries; OS-specific argv limits can still be lower. |
| Scheduler input | Name 200 bytes, prompt 65536 bytes, operation ID 256 bytes | Byte limits, not character limits. Error UX and cross-language consistency need to retain that distinction. |
| Completion summary | Error text truncated to 512 characters | Agent notification can omit trailing diagnostic detail. |

## Performance defaults, not correctness guarantees

| Area / source | Current values | Effect |
| --- | --- | --- |
| Catalog (`session_catalog/paging.rs`, Desktop client, TUI picker) | Default/page 50; maximum 100 | Bounded queries; frontend defaults are repeated across languages. Prefer documenting one protocol maximum and testing client conformance. |
| Catalog delta retention | 4096 revisions | An older cursor resets to bounded initial pages. Does not delete Sessions. |
| Reducer read cache (`message_log/read_state.rs`) | 8 Sessions / 64 MiB estimated cost; estimate is serialized record size times 3 plus 512 bytes per record | Eviction reconstructs the selected Session. Estimate is conservative and is not measured resident memory; no durable Core checkpoint exists. |
| Content offset cache (`message_log.rs`) | 8 Sessions / 100000 event+call entries | Oversize indices are not cached; selected-session indexing may repeat. |
| SQLite catalog | 5-second busy timeout | Writer contention may delay a request; does not make queries independent of all data sizes. |
| Desktop Runtime connection | Startup deadline increased from approximately 5 seconds to 30 seconds; retry every 100 ms | Failure allowance, not an intentional startup delay. Existing request timeout remains 30 seconds. |
| Polling | Catalog 5 s; task list 1500 ms; task output 500 ms after read; PTY output 200 ms; process completion worker 250 ms; scheduler 1 s | Latency versus background work. Prefer change-driven updates where justified; no claim these exact values are user-approved. |
| PTY interaction | Resize debounce 80 ms; process/PTY cleanup deadline 5 s; worker waits 25/100 ms | Implementation scheduling choices; cleanup deadline must continue reporting incomplete cleanup truthfully. |
| Transcript recovery | 50 ms retry yield; background observer waits 250/500 ms | Coordination timing, not proof that a run has completed. |

## Previously confirmed behavior

- Three retained file tabs plus a fourth replaceable preview slot; opening a
  retained file swaps with the preview as requested.
- Approximately equal main/detail width, draggable separators and no blue hover.
- TUI spacing and message markers were explicitly discussed with the maintainer.
- Five-field cron is the selected cron syntax, not a history/resource quota.

## Review boundary

The two cumulative durable-history ceilings above are actionable publication
findings, not merely opportunities to rename constants. Raising their values
would postpone the failure without resolving it. Retention changes must protect
pending delivery, active work and operation deduplication, and need failing
behavioral tests before implementation.

The numeric pass also found duplicate catalog indexes with identical columns
(`catalog_recent`/`catalog_pinned`, `catalog_workspace`/`catalog_workspace_pinned`).
They add write/storage work; inspect query plans and add an appropriate check
before removing them. Windows short-path versus canonical-path scope equivalence
is not covered by the catalog smoke test: it deliberately uses the canonical cwd
returned by `session/new`, as normal workspace selection does.

Local source checks and the 100 MiB unrelated-history test passed before this
review. The real two-client catalog smoke also passes, using canonical workspace
identity. These checks establish observed behavior, not approval of the values
listed here or a complete cross-platform release acceptance.
