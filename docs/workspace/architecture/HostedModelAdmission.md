# Hosted model gateway boundary

Core owns model and task semantics through `ModelClient`. Hosted Runtime calls
Django `/internal/model-runs`; Django owns provider credentials, request
validation and OpenAI/Anthropic SDK adaptation. The configured upstream may be a
model gateway or a direct provider. Credentials do not require an application
quota-domain binding.

Supplier-account concurrency, request/token quotas and shared cooldown belong
to the model gateway or upstream operator. Django does not maintain account
slots, quota advisory locks, shared cooldown records or a polling queue for
provider admission. Gateways must count streaming attempts until their streams
close; an RPM limit alone is not a concurrent-request limit. Direct-provider
deployments must supply their own upstream quota policy.

Application resource protection remains independent: Run admission and Runtime
execution/residency limits, HTTP capacity, PostgreSQL pools, upload occupancy
and storage stream lanes retain their existing contracts. Removing supplier
quotas does not make these controls account-wide model quotas or a complete
global limit on every model endpoint. API workers/connections and gateway
ingress limits must also fit the deployment's memory and connection capacity.

SDK clients retain `max_retries=0` and the configured provider timeout. A
cancelled synchronous call is rejected before provider dispatch; streaming
cancellation closes its provider iterator/client and records cancellation.
Ordinary ORM leases are returned before network waits, while caller-owned
transactions remain intact. This does not promise immediate interruption of an
already-running synchronous SDK request; it remains bounded by its timeout.

Provider status and `Retry-After` remain on `ModelProviderError`. Django does not
sleep or automatically resend after an upstream rejection. Runtime retains its
existing proxy retry loop: the current proxy error body does not forward
`Retry-After`, and ambiguous network failures can still cause a later external
attempt. No exactly-once provider execution is promised.

Runtime can opt into process-local model RPC admission through the paired
`HOSTED_MODEL_RPC_ADMISSION_DOMAIN` and `HOSTED_MODEL_RPC_MAX_IN_FLIGHT` settings
([configuration](../reference/Configuration.md#optional-model-rpc-admission)).
Both are absent by default. Runtime owns a fixed explicit domain table and gives
all clients in its selected domain the same Core `ModelAdmission`; client clones,
continued Runs and native subagents share it. Domain labels are host decisions,
never inferred from credentials, provider names or model names. A matching label
in another Runtime process does not share this capacity.

Each actual Runtime-to-API attempt holds a Core `ModelAttemptTicket` through the
complete JSON body or SSE stream. It releases the ticket on return, error or
future cancellation, before the existing retry backoff; queued cancellation
removes its queue entry without dispatch. This release concerns only this
process's RPC capacity. It does not prove that remote model computation stopped,
that a provider cancelled the request, or that any charge or quota was refunded.
The guard introduces no model responsibility ledger, distributed quota, Session
API field, or execution/residency memory budget.

Retained transcript failures keep their original diagnostic codes. Retired
quota reasons use the generic failure label rather than instructing operators
to configure the removed quota domain. Runtime no longer gives those reasons a
quota-specific classification; they retain the nonretryable unknown-error path.

Migration `0013_remove_model_quota_domain` removes the credential binding and
obsolete quota table, preserving credentials (including encrypted secrets and
versions), model configuration and host-capacity records. Historical migrations
remain unchanged. Stop intake and drain model calls before applying the forward
migration and switching API binaries; old binaries require the removed schema.
Reversing the migration recreates an empty quota table and null bindings, not
the discarded account policy. Restoring old behavior requires a backup or
explicit operator configuration; a schema rollback alone is insufficient.

`test_model_gateway.py` covers credentials without quota configuration, upstream
failure metadata, no local retry/cooldown and pre-dispatch cancellation.
`test_model_database_leases.py` exercises real HTTPS calls and disconnects with
a one-connection ordinary pool. The migration test upgrades populated legacy
credentials and verifies retained models and upload occupancy.
