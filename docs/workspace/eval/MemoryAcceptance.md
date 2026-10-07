# Memory acceptance

Memory is an existing private Markdown protocol. This acceptance distinguishes
model discovery, tool transport, storage persistence, and real-model behavior.
It does not add a background model or a second memory store.

## Offline discovery and storage checks

Run from Workspace with the pinned Core dependency resolved by Cargo:

```powershell
cargo test --locked -p runtime_server memory -- --nocapture
cargo test --locked -p runtime_server skill_projection -- --nocapture
python scripts/memory-protocol-acceptance.py --image <local-current-execution-image>
```

The script requires an explicitly selected existing local image. It resolves its
immutable ID, compares the installed memory skill with this checkout, creates a
random disposable volume, and uses network-disabled, read-only containers with
fresh `/mnt/data` tmpfs mounts. It never accepts a deployment volume name. The
volume and any remaining test containers are removed in `finally`.

The new `memory_discovery_reaches_the_model_request_without_plugins_or_eager_reads`
test runs the real Core request construction through Workspace's system-skill
configuration with zero plugins. Its offline ModelClient captures exactly one
request and stops before any provider access. The request contains the Memory
name, relevance/remember/update/forget description, readable SKILL.md location,
and ordinary `read`, `edit`, `write` definitions. Only the ordinary AGENTS.md probe
is permitted during construction: neither private Memory nor the skill body is
loaded eagerly. This verifies discoverability, not a model's decision to use it.

Routing is owned by existing adapters: DockerExecutionHost validates the canonical
URI, sends `filesystem-once` as the scoped Memory owner, and the hosted helper
routes it to `run_memory_file_system_operation`. Core keeps that identity opaque.
The script exercises the real helper protocol, including:

- Empty root discovery and an absent MEMORY.md.
- Reading the installed skill through the same filesystem helper.
- Writing a synthetic topic before its index.
- Reading both files from fresh containers sharing only the Memory subpath.
- Updating with the read observation hash and rejecting a stale hash.
- Separate synthetic user/Agent subpaths returning NotFound.
- Removing the preference and index contents and reading back empty files.
- Denying DeleteFile without changing the stored content; rejecting malformed URIs.

The Runtime scope test independently verifies that the actual user+Agent binding
is stable and distinct for a different user or Agent, and that its writable Docker
subpath mount is exact. Session ID is not part of this binding. The script uses
synthetic subpaths; it does not claim to have created real authenticated Sessions
or exercised the full deployed API/Runtime/Docker stack.

Forgetting a fact means editing/removing its topic content and index entry. Empty
files may remain. File deletion is unsupported by the current Memory protocol;
fixture-volume removal is cleanup, not evidence of a supported user delete tool.

## Recorded focused acceptance, 2026-09-20

Source: Workspace baseline `a6fc407`, Core `e674eb4`, with the Memory acceptance
test added locally. No Runtime or Memory production behavior was changed.

- Runtime `memory`: 4 passed, including actual offline model-request capture,
  stable private mount binding, zero-plugin discovery and mutation-fact acceptance.
- Runtime `skill_projection`: 2 passed.
- Linux hosted_execution `memory`: 2 passed, including atomic concurrent guarded
  writes. Run with `cargo test --release --locked --offline -p hosted_execution memory`.
- Core focused `query_loop`: 28 passed.
- `cargo fmt --all -- --check`: passed.
- Protocol script: passed using 26 fresh fixture containers, zero real model calls;
  both forgotten files were empty before the fixture volume was removed.

The pre-existing local general image rejected the current `observedFileHash`
field because it contained the previous internal protocol. This was a test
source/image mismatch, not evidence that the deployed matching version cannot
store Memory. No compatibility alias was introduced. A dedicated test image used
the execution helper built from the above source with the installed system skills:
`sha256:0b288bd8fa3760c890c0bf26b71f4a5264f2bfcc4b6c3949879da8c33566da7d`.
The installed skill's normalized SHA-256 was
`9093439a7262b2f01d7f34f22f3c456691f7df682dc273c22b3fc0588f16d022`.
Deployment containers, tags, memory volumes and release workflows were unchanged.

`cargo clippy --locked -p runtime_server --all-targets -- -D warnings` is blocked
by unchanged baseline findings: `too_many_arguments` at main.rs:1917,
`collapsible_match` at main.rs:3298, two `useless_conversion` findings at
postgres_store/integration_tests.rs:3013 and :3288, and two
`cloned_ref_to_slice_refs` findings at :3394 and :3428. No new test finding was
reported. Full release CI and performance experiments were not run.

## Bounded real-model follow-up, not executed

Proposed model: the repository catalog's `openai.default / gpt-5.6-luna`, low
thinking. Confirm its availability and applicable provider pricing when arranging
the run; no external service or credential was accessed for this acceptance.
Use fresh synthetic identities U1/U2 and Agents A1/A2; never demo Memory.

1. U1/A1, Session 1: explicitly remember “For this test I prefer green headings.”
2. U1/A1, new Session 2: ask which heading color was remembered, without supplying it.
3. U1/A1, Session 2: explicitly change the preference to blue.
4. U1/A1, new Session 3: explicitly forget the test heading preference.
5. U1/A1, new Session 4: verify that no saved heading preference remains.
6. U1/A2, new Session: ask for the saved preference; no U1/A1 content may appear.
7. U2/A1, new Session: ask for the saved preference; no U1/A1 content may appear.

Limits: at most 7 AgentRuns and 35 provider requests total, with retries counted;
at most 8,192 input and 1,024 output tokens per request; hard total spend ceiling
US$5. The operator must enforce/authorize the budget before dispatch, including
worst-case cost of the next request. Stop at the first unmet limit or unexplained
failure. Do not retry with another model or expand the experiment automatically.

For every step, retain the discovery metadata, tool path and successful mutation
receipt/readback. A verbal “remembered” is insufficient. Classify a failure as
missing discovery, instruction noncompliance, tool routing, authorization or
persistence before changing behavior. Finally clear all synthetic topic/index
contents through supported tools and dispose only the test identities/scopes.
This live adherence and authenticated cross-Session acceptance remains unverified.
