# Centaeris Agent SDK

`@centaeris/sdk` connects TypeScript hosts to the local Session JSON-RPC v1
protocol. The root entry is transport-independent. `@centaeris/sdk/node` owns
binary Node readable/writable streams and supplies JSONL framing for sockets or
child-process pipes. The package exports TypeScript source for Vite and Node
22.21 or later.

```ts
import { connect } from "node:net";
import { SessionClient } from "@centaeris/sdk";
import { createJsonlTransport } from "@centaeris/sdk/node";

const socket = connect(runtimeEndpoint);
const client = await SessionClient.connect(
  createJsonlTransport(socket, socket),
  { viewerId: "my-host-viewer" },
);

const stopListening = client.onUpdate((update) => {
  console.log(update.sessionId, update.agentRunId, update.params);
});
try {
  const session = await client.createSession({
    operationId: "create-my-session",
    cwd: process.cwd(),
  });
  await session.subscribe();
  const receipt = await session.send({
    operationId: "send-my-prompt",
    message: "Explain the repository",
  });
  console.log(receipt.sessionId, receipt.agentRunId, receipt.turnId);
  // Closing this client leaves the admitted Run with the Runtime.
} finally {
  stopListening();
  client.close();
}
```

Supply an endpoint obtained from the existing local host discovery/launch
mechanism. Each connection registers an explicit viewer with `clientKind: "sdk"`
by default. Desktop and TUI hosts can select their corresponding client kind.

`create` and `send` preserve the caller's operation ID. `send` resolves with the
durable admission receipt. `cancel` resolves with cancellation admission and the
current Run summary. The host observes later `session/update` notifications for
execution progress and terminal state. `load` reads persisted Session data.

`createSession` and `openSession` return an `AgentSession` bound to this client.
The object supplies its Session identity for load, send, subscribe, cancel,
supplement, question answers, context compaction and Run queries. Bound input
objects reject caller-supplied Session identities and unknown fields. Its Run
results expose runtime state separately from the raw protocol's product fields.

`resumeSession(sessionId)` loads the existing Session and reattaches the viewer.
It returns `{ session, subscription }`, including the existing Run identity when
one is available. The host advances recovery through its existing execution and
checkpoint lifecycle. Applications reconcile uncertain create and send outcomes
with their original operation IDs.

| Operation | Raw client | Bound Session |
|---|---|---|
| Supplement active input | `supplement(request)` | `supplement(agentRunId, message)` |
| Answer a question | `answerQuestion(request)` | `answerQuestion(answer)` |
| Request convergence | `answerNow(intervention)` | `answerNow(intervention)` |
| Compact context | `compactContext({ sessionId })` | `compactContext()` |
| Query Runs | `listRuns(request)` | `listRuns(includeTerminal)` |

Answer-now interventions use `agent_run.intervention.v1` with `kind` set to
`answer_now`. A bound Session confirms the Run's Session identity before sending
the intervention. The control methods preserve the host's existing acceptance
and error semantics. Applications choose when to reconcile failed or uncertain
operations through the command reliability contract.

`subscribe` attaches this connection's registered viewer to the requested Session
or Run. `onUpdate` subscribes locally to notifications and should be installed
before sending when output can precede an RPC response. Its cleanup removes that
listener. Connection closure releases the viewer through the Runtime lifecycle.
Accepted Runs belong to the Runtime and can continue after this connection closes.

Notification `params` remains the canonical opaque JSON value. Identity accessors
read explicit `sessionId`, `agentRunId`, and `turnId` fields from the outer value or
its `event` envelope. Runtime event task IDs retain their original meaning.

RPC failures reject with `RpcError`, preserving numeric code and optional data.
Invalid frames reject pending requests with `RpcProtocolError`. Connection
closure or send failure rejects pending requests with `RpcConnectionClosedError`.
Initialization and Session method responses are decoded against their exact v1
DTO fields, including nested messages and Run summaries. Missing, extra,
incorrectly typed and legacy fields are rejected. Canonical Core event content,
`streamItems`, image data and watchdog values retain their opaque JSON payloads.
Applications retain their operation IDs when deciding how to reconcile an unknown
outcome through the existing command reliability contract.

Run `pnpm --filter @centaeris/sdk typecheck` and
`pnpm --filter @centaeris/sdk test` for the package checks.
