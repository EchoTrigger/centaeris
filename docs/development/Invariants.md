# Ownership and invariants

This index points to existing contracts and their checks. The linked contracts
own the details. Test pointers identify coverage, not a claim that the tests
passed for the current revision. Use [Testing](Testing.md) to select validation
for a change and report the commands actually run.

## INV-01 Core owns runtime semantics

Owner: `packages/core`. Hosts adapt storage, transport, and execution without
defining another runtime state machine.

- Contract: [runtime ownership](../architecture/Architecture.md#runtime-ownership).
- Protection: Core's [query loop tests](../../packages/core/src/runtime/query_loop.rs)
  exercise continuation and tool routing. Run `cargo test --locked -p centaeris-core query_loop`.
  The [source gate](../../scripts/ci.py) also checks Core's resolved dependency tree for SQLite adapters.
- Limit: [boundary guardrails](../../packages/core/src/boundary_guardrails.rs)
  still mix dependency checks with source spelling and file layout assertions.
  These checks do not prove that every adapter preserves Core semantics.

## INV-02 Hosted execution requires a current Job lease

Owner: Workspace Runtime's lifecycle executor and PostgreSQL Job store.

- Contract: [Run lifecycle dispatch](../workspace/architecture/Architecture.md#run-lifecycle-dispatch).
- Protection: [lease handoff tests](../../packages/workspace/runtime/src/lifecycle_executor/lease.rs)
  cover an in-flight renewal, fence loss, and an uncertain acknowledgement.
  [PostgreSQL renewal tests](../../packages/workspace/runtime/src/postgres_store/integration_tests/lease_renewal_tests.rs)
  check that an old client clock cannot revive an expired lease.
- Validation: `cargo test --locked -p runtime_server lifecycle_executor::lease::tests`;
  `uv run --frozen --package api python scripts/workspace/runtime_outbox_gate.py --test-filter lease_renewal_tests`.
  The database gate requires isolated `TEST_POSTGRES_*` settings and owns its disposable database.
- Limit: unit lease windows do not prove database fencing or physical container termination.

## INV-03 Django owns hosted identity and admission

Owner: Workspace API. Admission binds the accepted operation to authorized
Session, Workspace, model, and resource identities.

- Contract: [hosted request flow](../workspace/architecture/Architecture.md#request-flow).
- Protection: [authorization flow tests](../../packages/workspace/api/app_core/test_authorization_flow.py)
  bind each Run identity; [hosted operation tests](../../packages/workspace/api/app_core/test_hosted_operations.py)
  reject replay after resource deletion or membership revocation.
- Validation: `uv run --frozen --package api python packages/workspace/api/manage.py test app_core.test_authorization_flow app_core.test_hosted_operations --settings=api.test_settings --noinput`.
  Use isolated `TEST_POSTGRES_*` settings.
- Limit: authorization fixtures mock resource lookup; transaction tests isolate Runtime scheduling.

## INV-04 Live delivery cannot replace durable facts

Owners: Runtime persists execution facts; Django projects them to SSE consumers.
Redis holds transient delivery state.

- Contract: [durable and live truth](../workspace/architecture/Architecture.md#durable-and-live-truth).
- Protection: [transient stream tests](../../packages/workspace/api/app_core/test_transient_stream.py)
  reconcile a durable terminal event and its committed cursor during simulated Redis OOM.
- Validation: `uv run --frozen --package api python packages/workspace/api/manage.py test app_core.test_transient_stream.TransientStreamTests --settings=api.test_settings --noinput`.
  [Real Redis acceptance](Testing.md#focused-core-gate) separately covers retention and expiry.
- Limit: the API fault fixture mocks storage and Redis. It does not prove deployed proxy or reconnect behavior.

## INV-05 An uncertain response does not authorize blind replay

Owners: each command receiver persists its receipt; clients recover the accepted
identity before resubmitting covered commands.

- Contract: [command reliability](../architecture/RuntimeCommandReliability.md#verified-current-behavior).
- Protection: the [local Runtime receipt regression](../../packages/cli/runtime/src/agent_runtime.rs)
  reopens Runtime after response loss and verifies the original Run and one committed user message.
- Validation: `cargo test --locked -p centaeris-cli-runtime completed_prompt_operation_replays_after_response_loss_and_runtime_reopen`.
- Limit: receipt coverage applies to the documented commands, not every mutation
  or power-loss scenario. [Hosted retry investigations](SourceUnification.md#deferred-retry-investigations)
  remain unverified observations, not established behavior.

## INV-06 Resource lifetime follows its owner

Owners: services own tool-concurrency registries; executors retain their scope
coordinators independently of client connections.

- Contracts: [state ownership](../../CONTRIBUTING.md#state-ownership-and-lifecycle)
  and [tool concurrency ownership](../architecture/Architecture.md#tool-concurrency-ownership).
- Protection: [concurrency tests](../../packages/core/src/tool/concurrency.rs)
  cover scope isolation, executor-held capacity, and registry teardown.
- Validation: `cargo test --locked -p centaeris-core tool::concurrency::tests`.
- Limit: these checks cover process-local coordination. Host-specific cleanup and
  persistent resource recovery need their owning adapter's tests.

## INV-07 Wire identities and persistent transitions are explicit

Owners: protocol types define wire identities; each storage adapter owns its migrations.

- Contracts: [wire rules](../../CONTRIBUTING.md#development-and-validation),
  [JSON-RPC envelope](../reference/RuntimeProtocol.md#json-rpc-envelope), and
  [lease-storage upgrade](../workspace/operations/RuntimeLeaseRecovery.md#upgrade-and-rollback).
- Protection: [RPC contract tests](../../packages/cli/runtime/src/runtime_rpc/contract.rs)
  reject unknown JSON-RPC envelope fields;
  [SQLite migration tests](../../packages/sqlite/src/sqlite_store/sqlite_schema.rs)
  preserve jobs, outbox, and migration history and reject unknown previous shapes without mutation.
  [Existing generated schema gates](SourceUnification.md#product-gates) protect hosted parity.
- Validation: `cargo test --locked -p centaeris-cli-runtime request_is_exact_and_uses_camel_case`;
  `cargo test --locked -p centaeris-runtime-sqlite lease_reclaim_`.
- Limit: the documented RPC `params` decoder accepts omitted or null values and
  ignores siblings of `request`; strict typed payloads do not imply strictness at
  every wrapper. The migration fixture covers one transition with in-memory SQLite,
  not every released format.
