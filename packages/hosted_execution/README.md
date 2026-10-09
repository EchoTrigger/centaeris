# Hosted Execution

`packages/hosted_execution` (`hosted_execution`) provides the hosted execution
agent and sandbox tool implementations that run inside each AgentRun's
temporary container. Its `execution_agent` binary is the in-container process
[`packages/runtime_server`](../runtime_server/README.md) drives through Docker
Engine exec streams for `read`, `bash`, `edit`, `write`, and related fixed
helpers; it receives neither the Docker socket nor control-plane credentials.

This package does not redefine Core's tool or execution semantics — it is the
sandboxed execution surface Runtime Server's container lifecycle binds those
semantics to.

See [Runtime Server](../runtime_server/README.md) for the container lifecycle
and execution boundary, and the [security model](../../docs/workspace/security/Model.md)
for trust boundaries and execution risk.
