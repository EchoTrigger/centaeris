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
`E2B_ADAPTER_IDEMPOTENCY_SUPPORTED=true` permits create only after
the chosen server's idempotency support has been verified. Credentials are
never sent into a sandbox or included in response diagnostics. The adapter
disables SDK action and connection retries; the caller manages durable retries.

Each stdin line is one bounded UTF-8 JSON request:

```json
{"schema":"runtime.e2b.adapter.request.v1","requestId":"request-id","operation":"inspect","payload":{"sandboxId":"provider-id"}}
```

Each stdout line is a response with schema
`runtime.e2b.adapter.response.v1`, the same `requestId`, `outcome`, and either
`result` or `error`. Unknown fields, duplicate JSON keys, and unsupported schemas
fail before dispatch. Template IDs use the signed profile's canonical identifier
grammar; build IDs must be immutable lowercase UUIDs, never moving aliases.

| Outcome | Meaning |
| --- | --- |
| `succeeded` | The requested SDK operation returned its described result. |
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
| `create` | `attemptId`, `idempotencyKey`, `templateRef: {templateId, buildId}`, string `metadata`, `timeoutSeconds`, `allowInternetAccess`, `onTimeout` (`kill` or `pause`) | Ordinary only. Sends `templateId:buildId` and `Idempotency-Key`; binds `centaerisAttemptId` metadata. Reports provider info and exact sandbox ID. |
| `connect` | `sandboxId`, `timeoutSeconds`, `onResume: "restore"` | Ordinary only. Explicitly allows provider resume; never hidden inside another operation. |
| `inspect` | `sandboxId` | All lanes. SDK control-plane information, without connecting or resuming. |
| `kill` | `sandboxId` | Cleanup only. Exact-ID delete acknowledgement; no physical settlement proof. |
| `pause` | `sandboxId`, `mode` (`full` or `filesystem`) | Cleanup only. Reports requested mode and provider acceptance, without asserting verified snapshot contents. |
| `readFile` | `sandboxId`, absolute `path`, optional `user`, `maxBytes` | Ordinary, explicitly connected sandbox. Streams bounded bytes and returns `dataBase64`. |
| `writeFile` | `sandboxId`, absolute `path`, `dataBase64`, optional `user` | Ordinary, explicitly connected sandbox. Includes empty binary files. |
| `runCommand` | `sandboxId`, `command`, `timeoutSeconds`, optional absolute `cwd`, `user`, string `envs` | Ordinary. Starts in background, returns PID, and disconnects the local output stream. |
| `waitCommand`, `recoverCommand` | `sandboxId`, `pid`, `timeoutSeconds`, optional `maxOutputBytes` | Ordinary. Connects to a known PID and returns completed output. Failure never reexecutes the command or proves non-admission. |
| `cancelCommand` | `sandboxId`, `pid` | Ordinary connected handle only. Reports SIGKILL request result for the PID; it is not process-tree cleanup evidence. |

The request limit is 1 MiB, file transfer limit 256 KiB, and combined command
output limit 64 KiB. An output limit disconnects the stream and returns unknown
completion with the PID retained. It does not silently kill or restart work.
At most 128 connected handles are cached; eviction does not delete a sandbox.

The public SDK has no verified no-resume attachment interface. A fresh control
process cannot acquire a command handle without potentially resuming an
environment, so it rejects that action. Run process-tree cancellation, storage
quotas, command-start idempotency, and full-memory pause verification are
explicitly unsupported. Retained lifecycle acceptance must remain gated until
its required process and retention semantics are demonstrated.

Run isolated behavioral tests without provider credentials:

```sh
uv run --frozen --package centaeris-e2b-adapter python -B -m unittest discover -s packages/e2b_adapter/tests -v
```

The SDK transport fixture exercises the real SDK against loopback HTTP; it does
not establish Firecracker, filesystem, process-tree, or retention acceptance.
