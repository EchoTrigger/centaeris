# Reproducible product acceptance

Start from a clean checkout of the revision under review. Record `git rev-parse
HEAD` and confirm that `git status --porcelain` is empty. Use the pinned tools,
synthetic accounts, and the fixture's mock endpoint. These paths use no paid
model requests. See [Testing](Testing.md) for the broader source and CI gates.

For each check, record the command, the scenario or test count, the observed
result, and the cleanup outcome. Use these result labels:

- **Build passed:** the candidate compiled. Product behavior is still unverified.
- **Behavior passed:** the intended test ran and produced the evidence below.
  Zero tests, skipped fixtures, `--help`, and `--version` do not meet this bar.
- **Failed:** the scenario executed and reported an error or missing evidence.
- **Blocked:** a prerequisite is missing. Record the failed preflight and the
  behavior that could not run.
- **Not run:** the check was outside this review. State separately which product
  behavior the check itself does not cover.

## Local CLI

On Linux x86-64, use the pinned Rust toolchain, a native linker, Python 3, and
Bash. The [Harbor native test setup](../../scripts/harbor/README.md#build-and-validate)
also shows how to build a Linux binary for Bookworm task images. For a native
Linux build, run from the repository root:

```sh
export CENTAERIS_RUNTIME_BINARY="$PWD/target/harbor-acceptance/debug/cet"
test "$(uname -s)" = Linux &&
  test "$(uname -m)" = x86_64 &&
  CARGO_TARGET_DIR="$PWD/target/harbor-acceptance" cargo build --locked -p centaeris-tui --bin cet &&
  test -x "$CENTAERIS_RUNTIME_BINARY" &&
  python3 -B -m unittest discover -v -s scripts/harbor -p test_native.py
```

The platform and executable checks must succeed. Require two executed tests with zero skips.
They launch the built `cet` over stdio. Check that shutdown exits with status
zero, the mock Run succeeds, the tool reads and then removes the expected image,
and the Runtime-owned background service remains alive after Run completion.
The test also checks committed usage records and the mock's tool and model
requests. A skipped test usually means the binary variable was missing. Check
the executable path and Linux loader before diagnosing RPC or model failures.

The fixture shuts down its child Runtime and removes its temporary profile and
workspace. These tests cover the Linux headless path. The
[Windows native execution smoke](../getting-started/Windows.md#verify-native-execution)
separately checks Windows Bash and file writes. Neither covers the packaged TUI
interaction or a real provider. [Core runtime ownership](Invariants.md#core-owns-runtime-semantics)
explains the behavior shared by hosts.

## Hosted Run

Use the [isolated performance fixture](../../perf/README.md#setup-and-smoke) on
an owned test daemon. Build from the checkout under review and verify that the
API, Runtime, native helper, PostgreSQL, Redis, and mock provider are ready.
`docker info` must report the intended daemon. Run the fixture's `check` to
validate configuration. Its `up` command verifies service health and mock TLS.
Use a pinned k6 binary with the SSE extension.

Sign in as the fixture Workspace owner and use Workspace model settings. Create
a provider with API `openai-responses`, base URL `https://mock-model:9999/v1`,
and any nonempty synthetic credential. Add an enabled model named
`perf-file-workload` with a 200,000-token context and an 8,192-token output limit.
Record the Workspace ID and that model's ID. For API setup, use the
[Workspace resource routes](../workspace/operations/WorkspaceResourceAdministration.md#workspace-resources):
`POST /api/workspaces/{workspaceId}/model-providers` returns `provider.id`, and
`POST /api/workspaces/{workspaceId}/models` takes that `providerId` and returns
`model.id`. Set `PERF_MODEL_REFS` to a JSON object of the form
`{"short":"<model.id>"}`.

Add these entries to the owned fixture's `perf/.state/test.env` before starting
its mock service:

```dotenv
PERF_MOCK_TOOL_NAME=bash
PERF_MOCK_TOOL_ARGS='{"command":"printf acceptance > /mnt/data/perf-result.bin"}'
```

In the k6 process, supply `API_BASE`, `PERF_ADMIN_EMAIL`,
`PERF_ADMIN_PASSWORD`, `PERF_WORKSPACE_ID`, and `PERF_MODEL_REFS` with the `short`
model ID. See [scenario inputs](../../perf/README.md#k6-scenarios). Leave
`PERF_RUN_FIXTURE_PATH` unset for a new submission, then run:

```sh
export PERF_ARTIFACT_EXPECTATION='{"filename":"perf-result.bin","sizeBytes":10,"sha256":"sha256:c1514c6cdad166c18aa557dc1d780ba84c0e32df7633235385023eb429377e74"}'
k6 run perf/k6/scenarios/run-smoke.js
```

Require one completed iteration, an accepted Run, successful paired tool calls
and results, a committed terminal event bound to that Run, and a download with
the expected bytes. HTTP 202 confirms admission alone. For an
admission error, inspect Workspace access and model configuration. If the client
loses the response or times out after admission, inspect the original operation
and Run before resubmitting. Confirm release after the test project drains.
The fixture owner can stop the owned project with
`python perf/harness/control.py stop`, which preserves its test volumes.

Check response-loss replay separately using the isolated `TEST_POSTGRES_*`
settings from [focused validation](../workspace/architecture/WebPresentation.md#focused-validation):

```sh
uv run --frozen --package api python packages/workspace/api/manage.py test \
  app_core.test_hosted_operations.HostedOperationTests.test_submit_response_loss_and_terminal_replay_ignore_mutable_dependencies \
  --settings=api.test_settings --noinput
```

Require one test to pass. The lookup returns the original accepted identity and
the test asserts one Run and one scheduling call. Django owns the disposable
database and the fixture substitutes Runtime scheduling. The smoke tests a
real Run but does not inject response loss or restart the stack. The
[API replacement smoke](../workspace/eval/ReleaseGate.md) separately checks
retention of seeded data after container replacement. See the contracts for
[hosted admission](Invariants.md#django-owns-hosted-identity-and-admission) and
[uncertain responses](Invariants.md#an-uncertain-response-does-not-authorize-blind-replay).

## Session

Use the [pinned Node and pnpm versions](Building.md#locked-toolchains). From the
root, install the frozen dependencies and run the focused Web tests:

```sh
pnpm install --frozen-lockfile
pnpm --filter web exec node --test \
  tests/unit/workspace-presentation-stream.test.mjs \
  tests/unit/operation-receipts.test.mjs
```

Check that both modules load and their tests run. A missing module is an
environment blocker. For the Django checks, prepare disposable PostgreSQL as
above. The local Runtime check only needs the pinned Rust toolchain.

```sh
uv run --frozen --package api python packages/workspace/api/manage.py test \
  app_core.test_presentation_stream app_core.test_session_workspace_retention \
  --settings=api.test_settings --noinput
cargo test --locked -p centaeris-cli-runtime --lib \
  completed_prompt_operation_replays_after_response_loss_and_runtime_reopen
```

Require safe replay when a stream disconnects before its checkpoint. The Web
fixture verifies that the client resumes from the applied cursor. The Django
fixtures verify replay of the committed terminal state and admission of a new
turn against the original Session, history, and workspace snapshot. The local
Runtime fixture reopens the Session and recovers the original Run and turn
without duplicating the user message. Use
[persistence acceptance](../eval/PersistenceAcceptance.md#local-work-history-and-admission-receipts)
for transcript rebuild and continuation checks. In failures, compare the
last applied cursor, Run and Session identities, and the terminal fact before
calling a repeated frame or a missing frame a product failure.

The tests own their temporary profiles and databases. They do not exercise the
deployed browser or proxy. For that check, open the Web UI of the owned Hosted
fixture and submit a message with a streaming mock model such as `perf-marathon`.
While output is streaming, set the browser's Network panel to Offline, then
restore the connection. Require the same Session and Run to reach the terminal
state with no duplicate message. Send a second message in that Session. Require
a new completed Run under the same Session and both turns visible after reload.
Let the fixture drain and stop it as described above. See
[durable facts and live delivery](Invariants.md#live-delivery-cannot-replace-durable-facts).

## Runtime

Use the pinned Rust toolchain and, for Hosted lifecycle checks, isolated
`TEST_POSTGRES_*` settings. At the repository root run:

```sh
cargo test --locked -p centaeris-core --lib \
  query_loop_same_run_recovery_uses_committed_compacted_context_before_new_input
uv run --frozen --package api python scripts/workspace/runtime_outbox_gate.py \
  --test-filter lease_renewal_tests
uv run --frozen --package api python scripts/workspace/runtime_outbox_gate.py \
  --test-filter resource_lifecycle_tests
```

Require one Core test to pass. It restores the committed context of the same Run before
accepting new input. The PostgreSQL tests must execute and demonstrate stale
lease rejection and resource release under the correct authority. If the gate
fails, inspect the selected test names and the lease, allocation, and cleanup
receipt for the original operation. An empty filter or setup failure supplies no
lifecycle evidence. The gate creates and removes its own database, with an
ownership check before cleanup.

The store tests do not demonstrate that a physical container stopped. To make
that claim, use the [owned native release gate](../workspace/eval/RunscRotation.md#run-an-owned-fixture)
and its cleanup evidence. See [Job lease authority](Invariants.md#hosted-execution-requires-a-current-job-lease)
and [resource lifetime](Invariants.md#resource-lifetime-follows-its-owner).
