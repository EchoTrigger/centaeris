# Hosted E2B SDK adapter

This package translates trusted Runtime requests to the pinned official Python
SDK (`e2b==2.58.0`). It owns no queue, allocation, lease, or settlement records.
The caller persists operation identities and responsibility before dispatch.

Run a persistent process for each bounded Runtime lane:

```sh
uv run --frozen --package centaeris-e2b-adapter python -m centaeris_e2b_adapter --lane ordinary
```

Use separate processes for `ordinary`, `control`, and `cleanup`: the SDK shares
its sandbox HTTP/2 connection pool within a process. A separate client object
does not provide lane isolation.

Host-side configuration requires `E2B_API_KEY`. Public deployments select
`E2B_DOMAIN`. Self-hosted deployments select both `E2B_API_URL` (control plane)
and `E2B_SANDBOX_URL` (commands and file traffic); this explicit pair works
without `E2B_DOMAIN`. For a local Embed installation, the pair can be
`http://localhost:3000` and `http://localhost:3002`. A sandbox URL is an endpoint,
not a domain, and is never inferred from the API URL. Both endpoints require
HTTPS except explicit HTTP loopback endpoints. Credentials, query strings and
fragments are rejected in endpoint URLs. A partial endpoint pair fails before
SDK construction, so file traffic cannot silently fall back to a remote host.

The CLI selects these values from host environment configuration and binds them
to the SDK client. Programmatic callers must select their endpoint overrides in
`AdapterConfig` too; unselected ambient SDK endpoint variables are rejected.
Create, connect and control operations inherit the same explicit client routing.
`E2B_ADAPTER_IDEMPOTENCY_SUPPORTED` is a configured capability declaration,
reported as `createIdempotencyConfigured`; it does not verify the server or grant
dispatch or retry permission. A stock Embed server can receive one fresh create
when this setting is false or unverified. Credentials are never sent into a
sandbox or included in response diagnostics. The adapter disables SDK action
and connection retries.

Before calling create, the Runtime must atomically claim a fresh durable attempt
and persist its identity and cleanup responsibility. Each admitted invocation
makes one SDK create call with the explicit idempotency key and immutable build.
The adapter owns no durable dispatch ledger and cannot enforce this admission
across requests or restarts. An unresolved attempt must not be reopened or sent
again because an idempotency header was supplied, a capability is configured, or
a lookup is empty. A lost reply or provider refusal remains Unknown for the
caller's investigation and settlement protocol.

Unknown provider failures retain a fixed, redacted message. When the pinned SDK
provides an HTTP status, `error.code` distinguishes 409 (`providerConflict`),
422 (`providerUnprocessable`), 429 (`providerRateLimited`) and 503
(`providerUnavailable`); other failures use `providerRequestFailed`. These codes
neither establish non-dispatch nor authorize retries or settlement. SDK 2.58.0
discards the provider body's semantic `error_code`, so a 409 cannot distinguish
`idempotency_in_progress` from `idempotency_request_mismatch`, and a 422 is not
reported as verified `idempotency_outcome_unknown`. Provider messages and bodies
are not returned, and these classifications do not verify server idempotency.

Each stdin line is one bounded UTF-8 JSON request:

```json
{"schema":"runtime.e2b.adapter.request.v1","requestId":"request-id","operation":"inspect","payload":{"sandboxId":"provider-id"}}
```

Each stdout line is a response with schema
`runtime.e2b.adapter.response.v1`, the same `requestId`, `outcome`, and either
`result` or `error`. Unknown fields, duplicate JSON keys, and unsupported schemas
fail before dispatch. Template IDs use the signed profile's canonical identifier
grammar; build IDs must be immutable lowercase UUIDs, never moving aliases.
Create `idempotencyKey` follows the [pinned provider OpenAPI contract](https://github.com/e2b-dev/runtime/blob/6b82caf690b06fb983254714187dc496ac4d0cec/spec/openapi.yml#L3533-L3548):
1 to 255 ASCII lowercase letters, digits or hyphens (`^[a-z0-9-]+$`). Invalid
keys fail before SDK dispatch regardless of configured idempotency support.
Attempt and sandbox identities retain their own broader bounded-string rules.

| Outcome | Meaning |
| --- | --- |
| `succeeded` | The requested SDK operation returned its described result. |
| `failed` | A read-only file operation received the pinned SDK's typed `FileNotFoundException`. The fixed `fileNotFound` code and original sandbox/transfer binding describe a completed failure, not a successful read. |
| `notDispatched` | Local validation or lane admission rejected this request before an SDK operation. This is evidence only for this invocation. |
| `unknown` | An SDK operation was attempted and the requested result is not proven. Known attempt, sandbox and PID identities are preserved when available. |

`notDispatched` does not settle an earlier unknown attempt. An empty or failed
lookup never proves removal. A successful SDK delete or provider not-found
response is reported as `deleteAcknowledged: true` for the exact sandbox ID.
This acknowledgement does not prove all physical cleanup or resolve unknown
creates. The caller must apply the selected provider's cleanup contract and
observed evidence, exclude unresolved creates, and account for other resources
before settling an environment. Create, connect and inspect reject provider
information for a different sandbox ID while retaining the original known ID.

| Operation | Payload | Result / constraint |
| --- | --- | --- |
| `capabilities` | `{}` | Limits and unsupported guarantees. Allowed in all lanes. |
| `create` | `attemptId`, `idempotencyKey`, `templateRef: {templateId, buildId}`, string `metadata`, `timeoutSeconds`, `allowInternetAccess`, `onTimeout` (`kill` or `pause`) | Ordinary only. Sends `templateId:buildId`, `Idempotency-Key`, and explicit `auto_resume: false`; binds `centaerisAttemptId` metadata. Reports provider info and exact sandbox ID. |
| `connect` | `sandboxId`, `timeoutSeconds`, `onResume: "restore"` | Ordinary only. Explicitly allows provider resume; never hidden inside another operation. |
| `inspect` | `sandboxId` | All lanes. SDK control-plane information, without connecting or resuming. |
| `kill` | `sandboxId` | Cleanup only. Exact-ID delete acknowledgement; no physical settlement proof. |
| `pause` | `sandboxId`, `mode` (`full` or `filesystem`) | Cleanup only. Reports requested mode and provider acceptance, without asserting verified snapshot contents. |
| `readFile` | `sandboxId`, absolute `path`, optional `user`, `maxBytes` | Ordinary, explicitly connected sandbox. Streams bounded bytes and returns `dataBase64`. |
| `writeFile` | `sandboxId`, absolute `path`, `dataBase64`, optional `user` | Ordinary, explicitly connected sandbox. Includes empty binary files. |
| `uploadFile` | `sandboxId`, absolute guest `path`, `transferId`, `byteLength`, `sha256`, optional `user` | Ordinary, explicit connection and private spool. Streams `<transferId>.upload` once, enforcing expected length and SHA-256. Returns integrity metadata. |
| `downloadFile` | `sandboxId`, absolute guest `path`, `transferId`, `maxBytes`, optional `user` | Ordinary, explicit connection and private spool. Streams into an exclusive new `<transferId>.download`; returns integrity metadata. |
| `runCommand` | `sandboxId`, `command`, `timeoutSeconds`, optional absolute `cwd`, `user`, string `envs` | Ordinary. Starts in background, returns PID, and retains the original output stream within the bounded command evidence cache. |
| `waitCommand`, `recoverCommand` | `sandboxId`, `pid`, `timeoutSeconds`, optional `maxOutputBytes` | Ordinary. Drains the original retained stream or replays a retained complete result. Returns exact `stdoutBytesBase64` / `stderrBytesBase64` and `stdoutByteLength` / `stderrByteLength`, exit code, and display text. Missing output evidence returns Unknown, without reconnecting or reexecuting the command. |
| `cancelCommand` | `sandboxId`, `pid` | Ordinary connected handle only. Reports SIGKILL request result for the PID; it is not process-tree cleanup evidence. |
| `invokeControlHelper` | `sandboxId`, `helper`, `expectedTemplateId`, `expectedMetadata`, `workspaceRoot`, `workspaceUser`, `timeoutSeconds`, `maxOutputBytes` | Control only. Fresh read-only provider attachment, then one fixed trusted helper with bounded raw output. No generic shell, connection/resume or TTL-extension request. |

The request limit is 1 MiB, inline file transfer limit 256 KiB, and combined command
output limit 64 KiB of original stdout and stderr bytes. The same raw-byte limit
applies to replay. An output limit disconnects the stream and returns unknown
completion with the PID retained. It does not silently kill or restart work.
At most 128 connected handles are cached; eviction does not delete a sandbox.

Bulk transfers are disabled unless the trusted host explicitly supplies
`AdapterConfig.spool_root` or the CLI's `E2B_ADAPTER_SPOOL_ROOT`. The host must
provision an existing absolute private directory owned by the Runtime account,
with only trusted writers. The adapter rejects symlink/reparse directory
ancestry and changed root identity; it does not inspect or change host ACLs.
Programmatic callers do not inherit the CLI's spool variable implicitly.

A host-generated canonical lowercase UUID selects only fixed `.upload` and
`.download` names under that root. Payloads cannot supply a local path. Guest
or model input must not choose transfer IDs. Upload sources must be regular,
single-link files, with matching size at admission. The SDK receives a binary
IO object whose reads are bounded to 64 KiB; length and hash are checked during
that one pass. Its regular-file descriptor preserves SDK size discovery without
seeking, buffering or rereading the source. This preserves the SDK's known-length
binary upload behavior on direct connections without relying on proxy buffering. The source stays owned by the host. An integrity failure after
SDK dispatch returns Unknown and may leave guest staging bytes: the caller must
use a private guest staging destination and must not invoke its helper unless
the successful length and hash match the owned request.

Downloads admit only an exclusive new regular file and write streamed SDK data
in slices no larger than 64 KiB. `byteLength` and `maxBytes` are bounded by
128 MiB; an upload may be empty. Results contain only `sandboxId`, `transferId`,
`byteLength`, and lowercase `sha256`, never a host path or inline file body.
This carries Core's bounded file/helper frames without Base64 expansion or a
per-chunk RPC protocol; it does not replace Core's own framing, authorization,
or snapshot-integrity validation.

Normal download failure closes its file and removes only the matching owned
partial file; preexisting and replacement files are preserved. Successful
spool files remain the caller's responsibility. Process death or a Runtime
transport deadline can leave a private partial, so the caller must retain its
transfer identity and reclaim it. The host also owns aggregate spool capacity
and file retention. These operations establish neither durable snapshot
storage nor physical storage quotas. SDK streamed uploads have no whole-upload
deadline; the Runtime's bounded adapter-process deadline remains required.
Capabilities report `bulkFileTransfer`, `maxTransferBytes`, and
`transferChunkBytes`; request and response JSON limits stay unchanged.

Command output uses the generated Process RPC client shipped with the exact
pinned official SDK, through its scoped connection configuration and sandbox
endpoint. This generated API is an internal SDK surface; upgrades require the
real protocol fixture to pass. The adapter caches the envd version from the
already required create/connect inspection and starts no extra inspection or
command connection to read output. It does not inspect CommandHandle internals
or introduce a guest launcher.

`commandOutputBytes: true` means a successful complete result preserves the
original stdout/stderr bytes, including NUL, invalid UTF-8 and code points split
across events. The existing `stdout` and `stderr` fields are display text,
decoded once as UTF-8 with replacement from each complete captured byte stream.
Consumers needing Core encoding diagnostics must decode the Base64 bytes with
Core's decoder; display text cannot establish original bytes or encoding.
An exited terminal event retains its exit code, including a nonzero exit;
provider error/status diagnostics are discarded. A missing exit event,
non-exited terminal event, malformed stream or disconnect returns Unknown with
the known PID, without fabricating successful completion.
Stream disconnection does not kill the process or establish tree cleanup.

Command recovery is limited to the current adapter process. A new provider
command connection can report exit status without replaying earlier output, so
it cannot establish a complete result. The adapter retains the original started
stream until completion or error, then disconnects it. It caches complete
stdout, stderr and exit code for repeated wait/recover requests. An adapter
restart, missing handle or evicted completion returns Unknown with
`commandOutputUnavailable` and the known sandbox/PID. It never returns empty
successful output merely because a fresh connection did not replay it.

Pending streams and completed receipts share 128 command evidence slots.
Admission can evict completed receipts; it never evicts pending streams to start
another command. When all slots are pending, the next start fails before SDK
dispatch. Reconnecting a sandbox does not replace its pending command streams.
The separate connected-sandbox cache still requires an explicit connect in this
process. Completed output must also fit a replay request's output budget.

The SDK fixes the original stream deadline at command start. A wait/recover
budget must cover that stream's remaining deadline; a smaller budget is rejected
before reading, while the pending handle and PID remain available. Runtime
transport deadlines independently bound each adapter call. The capabilities
response declares `commandOutputRestartRecovery: false`,
`commandOutputRecovery: "adapterLifetime"`, and
`waitBudgetMustCoverOriginalStream: true`. Durable guest output protection is
required before claiming recovery across adapter restarts.

Fixed control helpers support only the trusted template contract: workspace
`/mnt/data`, account `agent`, UID:GID `10001:10001`, and root-owned executable
`/opt/centaeris/bin/execution_agent`. The allowed modes are `prepare-workspace`,
`agent-process-state`, and `quiesce-agent-processes`. Preparation and observation
run as root; quiesce runs as `agent` because the existing helper requires the
workload identity and excludes its own PID. The working directory is always
`/`, and the two workspace environment variables are fixed internally. Other
workspace identities fail before provider access. Snapshot and filesystem
helpers are excluded from this inline control interface.

`expectedMetadata` must contain exactly the frozen creation keys
`centaerisAttemptId`, `environmentId`, `instanceId`, `executionId`, `agentRunId`,
`sessionId`, `workspaceId`, `profileDigest`, `authorizationDigest`, and
`processingJobId`. These refer to the original creation binding; they do not
replace the caller's current lease/controller authorization. The Runtime must
validate that authorization before requesting helper work.

A fresh control process uses the pinned SDK's generated read-only SandboxDetail
GET to recover the secure envd token in memory. Before starting any helper it
checks exact sandbox/template identity, an exact frozen metadata map, running state,
explicit timeout action `kill`, and literal `autoResume: false`. Missing,
ambiguous, paused or auto-resumable policy is rejected without starting a
helper. It neither constructs/connects a Sandbox execution handle nor calls
resume, create or set-timeout. The generated Process client uses the scoped
sandbox endpoint/token and fixed user; the provider API key is not forwarded
to the data plane. Tokens are not cached, persisted or returned. These internal
SDK surfaces are protected by the exact version pin and real protocol tests.

The helper timeout is 1 to 60 seconds; raw stdout and stderr share the 64 KiB
budget. Results preserve bytes, lengths and the known exit code, including
nonzero exits; callers must require exit zero and validate the existing helper
protocol before advancing. Lost replies, incomplete streams and output overflow
remain Unknown with the known PID. No helper retry or generic PID control is
introduced. As with ordinary commands, closing a stream does not prove process
or resource cleanup.

Generic control-lane commands, file access, connect/resume and PID cancellation
remain unsupported. Run process-tree cancellation, storage quotas, command-start
idempotency, and full-memory pause verification are also unsupported. A fixed
quiesce helper's UID-scoped result does not by itself settle an allocation or
establish retained task semantics. Retained lifecycle acceptance remains gated
until its required process and retention semantics are demonstrated.

Run isolated behavioral tests without provider credentials:

```sh
E2B_CONNECTION_RETRIES=0 uv run --frozen --package centaeris-e2b-adapter python -B -m unittest discover -s packages/e2b_adapter/tests -v
```

Start a fresh Python process with `E2B_CONNECTION_RETRIES=0` before SDK import,
matching the adapter CLI initialization. Test discovery may otherwise import the
SDK before an adapter exists; a pre-imported retry-enabled SDK is deliberately
rejected. A separate subprocess regression verifies that fail-closed behavior.
The SDK transport fixture exercises the real SDK against loopback HTTP; it does
not establish Firecracker, filesystem, process-tree, or retention acceptance.
