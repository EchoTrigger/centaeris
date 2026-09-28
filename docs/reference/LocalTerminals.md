# Local interactive terminals

Desktop opens Files, Review and Terminal from the right-panel + menu. Files and
Review reuse existing tabs; Terminal creates a new interactive shell. Existing
terminals can be reopened from the same menu. Closing or hiding a tab never stops
its shell. End terminal is the explicit termination action. Exited output remains
available until the Runtime's bounded retained-record cache evicts it.

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
most 16 chunks. There are at most 32 active terminals, 64 retained records and
4096 creation receipts per service instance. Eviction retains receipt tombstones
so a retry cannot silently create another shell. Input admission uses a bounded
32-message queue and bounded messages. Desktop also limits pending input, stops
sending after transport failure, and offers explicit reconnect without replaying
user input. Terminal control replies such as ConPTY's initial cursor-position
query remain enabled while replay is rendered.

The renderer uses xterm.js and FitAddon, consumes raw UTF-8 byte chunks, and never
passes terminal output through Markdown. Output eviction reports a gap and resets
the viewer; this is bounded replay, not a durable terminal-screen snapshot. Hidden
views do not send zero-sized resize requests.

## Lifecycle and limits

Active terminals keep the existing Runtime service alive after the last client
disconnects. Explicit Runtime shutdown requests termination and waits for bounded
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

After building the debug Runtime, run `node scripts/test-terminal-sessions.mjs`.
It uses an isolated profile, two real local clients and no model request, covering
PowerShell input/output, resize, duplicate start, stale-instance and operation
conflicts, Ctrl+C, all-client disconnect/reconnect beyond the idle timeout,
confirmed child creation before stop, and explicit service shutdown.

Rust/TypeScript share `packages/runtime/generated/terminal-samples.json` with a
Rust serialization parity check. UI tests cover control replies, no input retry,
and closing a view without stopping its process. Windows native verification is
performed locally; macOS/Linux native shell and cleanup acceptance remain required
on those platforms. Interactive full-screen applications and visual fit are in the
Desktop manual acceptance checklist.
