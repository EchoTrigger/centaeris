# Workspace Agent Worker

`packages/workspace/worker` delivers terminal Job outbox events and invokes
bounded platform reconciliation. Workspace Runtime owns lifecycle Job claim,
renewal, execution, waiting, cancellation and cleanup dispatch. The service no
longer starts Python execution or control dispatchers.

Session, model, tool and Runtime Job semantics remain owned by `packages/core`.
The remaining outbox and reconciliation loops use existing Runtime/API contracts.

See [Deployment](../../../docs/workspace/operations/Deployment.md) and
[Data](../../../docs/workspace/operations/Data.md) for required images, topology,
and persistence boundaries.
