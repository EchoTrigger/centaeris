# Native runsc acceptance utilities

See [the gate contract](../../../docs/workspace/eval/RunscRotation.md).

- `build.py` builds exact clean source and stages the static musl helper.
- `daemon.py` owns separate Docker/containerd processes and the raw memory probe.
- `isolation.py` verifies actual transport/root/namespace ownership.
- `prepare.py` writes a private native-only fixture without starting services.
- `model.py` returns bounded actual tool intentions over the OpenAI chat contract.
- `admit.py` uses public login/CSRF/admission; it creates no execution facts.
- `run_status.py` observes bounded read-only authoritative status.
- `seed.py` creates only synthetic business inputs in an empty fixture database.
- `launch.py` coordinates the raw prerequisite and full product gate.

Credentials remain in the owned root or Docker volumes and are never printed.
No utility changes the current application stack, shared Docker resources, or
operator runtime configuration.

For a shared eight-environment experiment, pass `--shared-workload --environment-count 8`.
Each environment keeps its 2-core, 4-GiB ceiling. The eight-environment fixture uses
12 GiB for both workspace and global measured-memory admission; the four-environment
baseline keeps its 8-GiB workspace budget. Storage budgets remain at least 32 GiB
and at least 4 GiB per environment. These are experiment parameters, not proof that
eight environments can simultaneously consume their full memory ceilings.

- `--model-wait-ms` adds 0–120000 ms of model-side wait before the fixed command.
- `--initial-stagger-ms` spaces initial environment admissions; the total initial
  stagger `(environment-count - 1) * initial-stagger-ms` cannot exceed 120000 ms.
- `--expected-working-environments` defaults to the full environment count. A sparse
  eight-environment experiment may explicitly require two working simultaneously;
  its report still records eight environments, threshold two, and the observed peak.

Model wait and initial stagger are recorded separately from Run latency. Every
planned environment still requires original-instance continuity, successful Run
terminal and cleanup facts, physical pause/resume, and positive product retirement.
The original single-slot rotation fixture keeps its existing parameters.

The durable timing report correlates successful tool results with an earlier tool
call in the same Run using exact `callId` and tool name. It reports execution-fact
and first successful result `createdAtMs`, plus `executionFactToFirstToolResultMs`.
These are timestamps carried by committed Session events, not measured database
commit instants, task process-start times, or resume-to-first-tool latency. Missing
or unconfirmed event shapes remain null; exact resume timing remains not observed.
