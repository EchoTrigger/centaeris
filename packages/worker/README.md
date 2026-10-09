# Workspace Agent Worker

`packages/worker` (`workspace-agent-worker`) is the hosted product's lifecycle
worker. It polls and leases `agent_run.lifecycle` and `worker.noop` jobs from
the internal Runtime/API job queues (`RUNTIME_INTERNAL_URL`,
`API_INTERNAL_URL`), advances AgentRun lifecycle state, and reconciles pending
work under bounded lease and recheck intervals.

The worker does not own Session, model, tool, or Runtime job semantics; those
remain owned by `packages/core` and are adapted by
[`packages/runtime_server`](../runtime_server/README.md). It is a lifecycle
driver over the existing authorized AgentRun API.

See [Deployment](../../docs/workspace/operations/Deployment.md) and
[Data](../../docs/workspace/operations/Data.md) for required images, topology,
and persistence boundaries.
