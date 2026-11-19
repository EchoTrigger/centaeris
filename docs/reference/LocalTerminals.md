# Local interactive terminals

The local Runtime owns each PTY, independently of Agent Session/AgentRun semantics.
A terminal records its originating workspace and optional chat ID; changing the
current workspace does not move a running terminal. Its recorded workspace is the
initial directory, not a claim about the shell's later `cd` location. Session
association is metadata, not ownership: deleting a chat does not kill user shells.

## Shell selection

An explicit shell executable in the create request wins, followed by the Host's
`CENTAERIS_TERMINAL_SHELL` environment variable. Windows resolves `pwsh.exe` from
PATH, falling back to `powershell.exe`. Unix uses a valid `$SHELL`, then the system
account's login shell, finally `/bin/sh`; Unix shells start as login shells. The
new shell inherits the Runtime environment, not variables/functions held privately
inside an unrelated interactive shell. This setting does not change Agent tools.

## Host protocol

`terminal_manage` accepts an exact `request` object with one action:

- `list`: `workspaceRoot`; returns `serviceInstanceId` and `terminals`.
- `start`: nested `request` containing `operationId`, `serviceInstanceId`,
  `workspaceRoot`, nullable `sessionId`, nullable `shell`, `cols`, `rows`.
- `read`: `target` and decimal-string `cursor`; returns `terminal`, `chunks`,
  `nextCursor`, `gap`, `hasMore`. Chunks contain `cursor` and `dataBase64`.
- `write`: `target`, `dataBase64`; returns `accepted: true` for queue admission,
  not proof that the shell executed a command.
- `resize`: `target`, `cols`, `rows`; returns `accepted: true`.
- `stop`: `target`; returns a snapshot; stopping is not an exit acknowledgement.

`target` contains `terminalId` and `serviceInstanceId`. All actions use the
no-automatic-retry transport policy. Explicit start reconciliation uses the same
operation ID and identical request. Input is never automatically resent. A stale
service instance fails rather than recreating a terminal. Read cursors provide
stateless attach/detach: viewers own no PTY lifetime or exclusive input lease.
Multiple viewers share the terminal; the most recent resize changes its geometry.

Each terminal retains at most 1 MiB / 4096 output chunks; read pages contain at
most 16 chunks. The default is 8 active terminals; a positive
`CENTAERIS_TERMINAL_MAX_ACTIVE` explicitly overrides it. The entry cache retains
up to 64 entries (or the configured active limit if larger), evicting only fully
ended terminals. Creation receipts live in indexed durable storage without a
cumulative admission ceiling, so eviction cannot make retries create another
shell. Input admission uses a bounded 32-message queue with blocking backpressure.
## Lifecycle and limits

Terminals live with the owning Runtime. Runtime shutdown requests termination and waits for bounded
cleanup. Windows reuses the Host's kill-on-close Job Object; Unix terminates
processes in the PTY session, including its foreground process groups. Ctrl+C is
ordinary PTY input, not the explicit stop operation. Before spawning on Windows,
the Host clears the inherited ignore-Ctrl+C flag from detached service launch,
so ConPTY can deliver interrupts to the shell. These are current-user
process facilities, not an OS sandbox; deliberately detached processes outside
the terminal's containment are not covered by an isolation claim.

A Runtime restart invalidates old identities; no machine-restart recovery, remote
PTY attachment, terminal-screen persistence, or Agent control tools are provided.
TUI background task behavior and Core contracts are unchanged.

## Validation

Run `cargo test --locked -p centaeris-cli-runtime terminal_sessions`. The
serialization fixtures are in `packages/cli/runtime/src/generated/terminal-samples.json`.
