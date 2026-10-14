# Environment task service acceptance

These checks cover the task supervisor component. They do not establish the
API/Worker/Runtime retained-environment lifecycle or process checkpoint recovery.

## Retained Run integration gate

Retained execution is opt-in; `perRun` remains the default. Its initial guarantee
is continuity within the same living physical instance. A stopped or missing
instance is not permission to create a replacement writer. It does not promise
process recovery after host loss.

Before publication, exercise the actual API, Worker, Runtime and Docker path:

1. Run A starts a registered Environment task and writes a workspace file.
2. A completes and its cleanup receipt commits. Its Run-scoped tasks stop, while
   the Environment task continues and the resident resource debit remains.
3. Restart the Runtime controller, then submit Run B in the same Session.
4. Require the same physical container, instance, background task and file bytes.
   Query the resource ledger: there must be exactly one physical resident debit.
5. Attempt to terminate that task using A's stale control fence. Require rejection
   at the task service, then confirm with B's current fence that the task survives.

Use a local model fixture to request tools, but do not simulate Run terminal
facts, task execution, cleanup receipts, instance identity or resource records.
Record the source revision and binary identities with the observations. Keep the
test database, image configuration and containers separate from existing product
deployments. Component tests alone do not satisfy this gate.

The retained path exposes registered background tasks and Run-scoped commands.
Old privileged file helpers, MCP dispatch and lifecycle hooks fail explicitly;
their retained equivalents are not part of this acceptance. Terminal file
protection is deferred while live tasks can write: completion must not quiesce
the whole instance or label an uncoordinated image as a recovery checkpoint.
Task output has bounded retention and reports cursor gaps. It is not an unlimited
log archive. Archive uploads and cold restoration require separate acceptance.

Run the ordinary `hosted_execution` tests on Linux. The ignored tests in
`task_service::linux::tests` additionally require an explicitly delegated,
disposable cgroup v2 subtree, supplied by `CENTAERIS_TASK_TEST_CGROUP_ROOT`.
Run them serially, as root inside the isolated fixture. Never supply the host
cgroup root or a production instance. Use a private, bounded `/mnt/data` and
temporary directory, an outer resource limit, and a wall-clock deadline.

The real-process checks cover controller takeover, Run-scoped versus
environment-scoped termination, output draining and exhaustion, lost input
receipts, and a child that creates a new process session. Completion requires
the owned task subtree to report empty; process-group termination alone is not
the acceptance condition.

## Observed Docker configuration

The real retained Run gate passed at source revision `a168010` using a local
deterministic HTTPS model and the real API, Worker, Runtime, PostgreSQL and Docker
path. [The observation record](retained-continuity-observation.json) includes
the exact source and binary identities, A/B Run and release identities, matching
instance/container/task identities and file hash, controller epochs, Runtime
restart identity, and the single resident allocation's resource budget.
Both Runs completed. A's stale query and termination were rejected at the task
service; a subsequent current-controller query confirmed the task still ran.
An earlier failed diagnostic Run had already started another environment task;
the evidence preserves both tasks rather than presenting a fresh-environment
experiment. Ordinary filesystem helpers and model-visible task identities were
exercised. The full regression result is pending CI.

The stale-operation probe invokes the task-service relay using the persisted
fence; it does not independently prove the outer Runtime HTTP authorization
checks. Those checks have separate PostgreSQL and protocol behavior tests.
The resource observation establishes a single ledger debit, not physical I/O,
WAL amplification, production capacity, or host-loss recovery guarantees.

The component revision `8c36124` passed 54 ordinary Linux tests and all eight
delegated cgroup tests. The eight delegated tests also passed inside a dedicated
Docker container with this topology:

```text
dedicated systemd slice
  Docker instance cgroup (aggregate limits)
    tasks (domain, no enabled subtree controllers)
      individual task domains
```

The test bound only the dedicated slice into the container. A trusted host
preparation step verified the container's actual cgroup through its inspected
PID before creating `tasks`. Tasks remained descendants of the Docker instance,
not siblings that would escape its accounting. The observed instance limits were
256 MiB memory, one CPU (`cpu.max = 100000 100000`), and 128 PIDs. The container
used a read-only root, no network, all capabilities dropped except `SETUID`,
`SETGID` and `CHOWN`, and `no-new-privileges`; neither privileged mode nor
`SYS_ADMIN` was enabled. The test did not intentionally exhaust these instance
limits and makes no throughput or production-capacity claim.

After the tests, the exact labeled test container was stopped. Its cgroup was
required to be absent or report `populated 0`; the exited container was retained
for inspection. The separate systemd holder used to establish the fixture's
parent slice was also stopped.

Separate Unix socket tests reject missing, truncated, oversized, malformed and
unsupported-schema responses without returning partial output as success. Real
child-process startup tests verify that a competing service cannot rewrite the
journal, that a stale socket fails before journal recovery, and that the control
lock lasts for the manager's lifetime without surviving in exec children.

This fixture runs the supervisor's process-management tests directly. It does
not exercise the production image entry point, the service socket relay, host
restart, resource-ledger reconciliation, or an Agent Run. Those remain separate
acceptance requirements before enabling retained admission. The dedicated-slice
setup is an experimentally verified configuration, not a deployed host helper.
