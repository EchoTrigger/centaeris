# Workspace Agent Worker

`packages/workspace/worker` invokes bounded platform reconciliation.
Workspace Runtime owns terminal outbox delivery, waiter recovery, lifecycle Job claim,
renewal, execution, waiting, cancellation and cleanup dispatch. The service no
longer starts Python execution or control dispatchers.

Session, model, tool and Runtime Job semantics remain owned by `packages/core`.
Platform reconciliation continues to use the existing authorized API contracts.

See [Deployment](../../../docs/workspace/operations/Deployment.md) and
[Data](../../../docs/workspace/operations/Data.md) for required images, topology,
and persistence boundaries.
