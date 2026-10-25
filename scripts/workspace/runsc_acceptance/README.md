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
