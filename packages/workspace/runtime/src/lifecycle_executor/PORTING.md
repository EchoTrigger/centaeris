# Lifecycle ownership transfer

| Python boundary | Native boundary |
| --- | --- |
| `claim_next_job`, `run_control_dispatcher` | Tokio `dispatch`, existing hosted/control claim transactions |
| `lease_heartbeats` | `lease::Lease`, existing database-clock renewal operation |
| `execute_agent_run_lifecycle_job` | `advance`, shared `agent_run_execution::step` |
| `finish_environment_agent_run` | `finish`, shared fenced cleanup and platform release |
| `execute_control_job` | `advance_control`, shared terminal-only control operation |
| `fail_job` | Shared typed `job_protocol::fail_owned_job` |
| `TerminalDispatcher` | `outbox::Dispatcher`, transactional PostgreSQL notification |
| Waiter reconciliation | `outbox::reconcile`, same durable checkpoint cursor |

The native dispatcher invokes execution and cleanup directly. Platform requests
remain HTTP authorization/projection boundaries. Waiting keeps the current lease
while active children use the parent's shared environment controller. Child Jobs
retain independent execution leases and cancellation. Durable wakes resume that
owner, with notifications for latency and timed checks for recovery. Other waits
yield the lease; the next claim receives a fresh owner. Unconfirmed handoffs remain reclaimable.
No terminal result substitutes for cleanup or release evidence. Retained mode
confirms Run task cleanup without requiring environment destruction.

The existing HTTP adapter and PostgreSQL fencing/receipt regressions still cover
the same operations. Native lease-window tests cover delayed acknowledgements
and fence loss. Handoff coverage waits for an in-flight renewer and rejects both
a confirmed fence loss and an uncertain acknowledgement before lease-ending
mutations. Native waiting tests preserve Runtime and authorization deadlines.
`test_native_lifecycle_runtime.py` exercises real Runtime/API/PostgreSQL execution,
renewal, cancellation, new-owner resume and independent control progress with no
Python execution dispatcher. Its physical host and model are owned test adapters;
these tests do not establish production capacity or model latency.

The Python package is retired. `app_core.platform_reconciler` retains only the
two Django business reconciliation calls, bounded pagination and safe diagnostics.
It receives no database or execution configuration. Existing controller pin and
exhausted-retry waiting scenarios now use Runtime's native dispatcher or the
canonical Runtime HTTP client.
