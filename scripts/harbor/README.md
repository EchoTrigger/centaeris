# Terminal-Bench adapter

This adapter requires Harbor 0.21.0, a Linux x86-64 Docker engine, and a prebuilt
Linux Centaeris Runtime. The adapter submits the task through the existing Runtime
JSON-RPC interface; Runtime and Core own model requests, tools and compaction.

`five-tasks.json` pins Terminal-Bench 2.1 and selects five tasks. Each receives five
independent attempts with an isolated Runtime profile, with at most two trials
running concurrently. Retries are disabled. The selected tasks declare 1 CPU and
2 GiB of memory each; the host also needs room for Docker, Harbor and builds.

Provider, model, reasoning and token limits come from local settings outside the
repository. For example, a 500,000-token context with 64,000 maximum output tokens
uses decimal
token counts. Output is reserved inside the total context, leaving 436,000 input
tokens. Core applies its existing compaction headroom (32,768 tokens here), so
compaction begins around 403,232 estimated input tokens. Catalog capacity is
unchanged. Runtime process environment variables constrain requests:

- `CENTAERIS_MODEL_CONTEXT_BUDGET_TOKENS`
- `CENTAERIS_MODEL_OUTPUT_BUDGET_TOKENS`

Unset variables preserve catalog defaults. Invalid, zero or excessive budgets,
and output budgets at least as large as context, fail loudly. These variables
apply to all model selections in that Runtime process.

## Build and validate

Build against Debian Bookworm (glibc 2.36), since several selected tasks use that
base. A normal Ubuntu 24.04 build can require a newer glibc than the task supplies.
From the repository root on Linux or WSL with Docker enabled:

```bash
mkdir -p target
docker run --rm \
  --mount "type=bind,source=$PWD,target=/source,readonly" \
  --mount "type=bind,source=$PWD/target,target=/build" \
  -e CARGO_TARGET_DIR=/build/harbor-bookworm -e CARGO_BUILD_JOBS=2 \
  -w /source rust:1.95.0-bookworm \
  /usr/local/cargo/bin/cargo build --locked -p centaeris-runtime --bin centaeris-runtime
export CENTAERIS_RUNTIME_BINARY="$PWD/target/harbor-bookworm/debug/centaeris-runtime"
python3 -B -m unittest discover -s scripts/harbor -p 'test_*.py'
```

Windows Docker Desktop can also build this Linux binary. Use absolute Windows
bind paths for the two mounts above. The output is still a Linux ELF, not `.exe`.
The `test_native.py` acceptance requires Linux and the binary environment variable;
it uses only a mock model and SOCKS5h proxy, and tests remote DNS, actual output
limits, reasoning selection, and survival of a Runtime-owned background process
after completion.

Install Harbor with `uv tool install 'harbor==0.21.0'`. Validate the Docker engine
with `docker info`; WSL must have Docker Desktop integration enabled for its
distribution. Windows Harbor can use the Windows Docker CLI directly.

The adapter forwards host `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` variables.
With Docker Desktop it rewrites loopback proxy hosts to `host.docker.internal`,
preserving the port and scheme, including `socks5h` remote DNS. Set
`CENTAERIS_BENCH_PROXY_URL` to override the container proxy explicitly. On a Linux
Docker host, use a host gateway address reachable from the container. The adapter
adds `127.0.0.1,localhost,::1` to `NO_PROXY` so task-local services bypass the proxy.
Keep proxy credentials in the process environment, outside job configuration.

For local Rust gates, set `NO_PROXY=127.0.0.1,localhost,::1` in the test process.
Credential-absence tests also require a test environment without ambient provider
API keys; do not remove credentials from the user's global environment.

## Run

The tracked task manifests contain dataset identities and scheduling only. They
select no provider or model. On Windows, `run.ps1` reads a local settings file:
`%LOCALAPPDATA%/Centaeris/benchmarks/settings.json`, or the path supplied through
`-SettingsPath` / `CENTAERIS_BENCH_SETTINGS`. Its fields are:

```json
{
  "providerId": "chosen-runtime-provider",
  "model": "chosen-model",
  "credentialEnv": "MODEL_API_KEY",
  "reasoningEffort": "max",
  "contextTokens": 500000,
  "maxOutputTokens": 64000
}
```

Choose a provider and model supported by Runtime. The launcher resolves private
Harbor job configurations beside this settings file; those files contain no key.
The adapter reads the environment variable named by `credentialEnv` and submits
credentials through Runtime's configuration protocol. Provider HTTP behavior
remains owned by Runtime. The container's isolated private Runtime profile can
persist its credential while running; it is removed with the container. Keys are
not interpolated into shell commands or written to result files.

On Linux, create a local Harbor configuration outside the repository from the task
manifest. Set `agents[0].model_name` and its kwargs `provider_id`, `credential_env`,
`reasoning_effort`, `context_tokens`, `max_output_tokens`. Export the chosen key
variable, `CENTAERIS_RUNTIME_BINARY` and `PYTHONPATH=$PWD/scripts/harbor`, then run
`harbor run -c /path/to/local-job.json`.

Windows PowerShell, from the repository root:

```powershell
.\scripts\harbor\run.ps1 -CheckOnly
.\scripts\harbor\run.ps1
```

The launcher first checks the configured key variable in the process environment,
then Windows User environment variables. It prompts for a hidden key only when
both are empty, launches a hidden background worker and restores the parent
process environment. Once it prints the worker PID, that terminal can close. Docker must
stay running and the host must stay awake. Use `-Foreground` to run interactively.
An existing pilot job resumes automatically; completed trials are retained.
Before resume, the job is backed up under `jobs/backups/`, and cancelled trials
are retried through Harbor's `CancelledError` filter.

The command schedules **25 paid attempts**. The adapter retains an initialized Runtime connection after successful
completion so Runtime-owned services survive the shared-container verifier.
Harbor container teardown releases that process; cancellation requests target the
admitted AgentRun and shut down its Runtime.

For an explicitly authorized pilot-review-then-full run, use
`.\scripts\harbor\run.ps1 -FollowWithFull`. After the pilot succeeds, the worker
keeps its inherited key in memory and records `stage=awaitingReview` in
`jobs/control/centaeris-tb21-five-tasks-pass5/phase.json`. A trusted reviewer must
inspect the results and atomically write `review.json` in that directory with
`{"decision":"runFull"}` or `{"decision":"stop"}`. The first decision runs
`full-tasks.json`: 89 tasks at the same pinned revision, five attempts each,
445 trials, two concurrent trials. Model mistakes are distinct from harness
failures. The worker times out if the review has not arrived after six hours.

Worker PID and output logs are under `jobs/control/centaeris-tb21-five-tasks-pass5/`;
`finished.json` records the eventual exit code. Each trial is removed after the
verifier and output collection finish (`environment.delete=true`); the next
queued trial then starts. Built Docker images remain cached.

Harbor 0.21 registers the default metric under `adhoc` for explicit task lists.
The pilot and full configurations therefore omit `TaskConfig.source`; task Git
identities still pin Terminal-Bench 2.1 exactly. A regression test resolves Harbor's
metrics and exercises the successful-reward path that previously crashed.

## Results

Harbor writes job and per-trial results below `jobs/`, including verifier rewards
and exceptions. Agent logs include `result.json`, the persisted conversation,
raw Runtime notifications in `events.jsonl`, Runtime logs, and a SHA-256 Runtime
build identity in Harbor agent metadata. Native Runtime logs are not ATIF format.
Token/cost fields are left unknown unless provided explicitly; they are not zero.

For this pilot, with exactly five attempts per task, pass@5 is the fraction of
tasks with at least one verifier reward of 1. Report infrastructure errors and
missing verifier results separately. A successful AgentRun alone is not a passed
task. Five selected tasks are a pilot, not the full benchmark leaderboard score.
