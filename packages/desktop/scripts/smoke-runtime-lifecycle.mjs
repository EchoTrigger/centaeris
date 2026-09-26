import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createRuntimeHostTransport } from "../src/runtimeHostTransport.mjs";

const terminal = (status) => ["succeeded", "failed", "cancelled", "stopped"].includes(status);

const eventually = async (read, predicate, label) => {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const value = await read();
    if (predicate(value)) return value;
    await delay(50);
  }
  throw new Error(`Timed out waiting for ${label}`);
};

// Uses the actual Desktop transport, a real Runtime process, and an independent
// client connection. Only the external model service is controlled by this test.
export const assertRuntimeLifecycle = async ({ runtimeExe, repoRoot, tempRoot }) => {
  const dataRoot = path.join(tempRoot, "runtime-client-lifecycle");
  const cwd = path.join(dataRoot, "workspace");
  const source = path.join(dataRoot, "source");
  await fs.mkdir(source, { recursive: true });
  const git = (...args) => execFileSync("git", args, { windowsHide: true, stdio: "pipe" });
  git("init", source);
  git("-C", source, "-c", "user.name=Lifecycle Test", "-c", "user.email=lifecycle@example.invalid", "commit", "--allow-empty", "-m", "Initial fixture");
  git("-C", source, "worktree", "add", "--detach", cwd, "HEAD");
  const requests = [];
  const heldResponses = new Set();
  const model = http.createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      requests.push(JSON.parse(body));
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.write(`data: ${JSON.stringify({
        id: "lifecycle-model",
        choices: [{ index: 0, delta: { role: "assistant", content: "Still working" }, finish_reason: null }],
      })}\n\n`);
      heldResponses.add(response);
      response.on("close", () => heldResponses.delete(response));
    });
  });
  await new Promise((resolve, reject) => {
    model.once("error", reject);
    model.listen(0, "127.0.0.1", resolve);
  });
  const environment = {
    ...process.env,
    CENTAERIS_DESKTOP_DATA_DIR: dataRoot,
    CENTAERIS_SYSTEM_SKILLS_SOURCE: path.join(repoRoot, "system-skills"),
    CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
    CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
    OPENAI_API_KEY: "", DEEPSEEK_API_KEY: "", KIMI_API_KEY: "", CENTAERIS_MODEL_API_KEY: "",
  };
  const servers = [];
  const clients = [];
  const events = [];
  const createClient = async (viewerId) => {
    const transport = createRuntimeHostTransport({
      executablePath: runtimeExe, cwd: repoRoot, environment,
      emitHostEvent: (eventName, payload) => events.push({ eventName, payload }), isAppReady: () => false,
      isQuitting: () => false, isSmokeRun: true,
      onRuntimeServerStarted: (server) => servers.push(server),
    });
    clients.push(transport);
    await transport.invokeCommand("initialize", { request: { clientKind: "desktop", viewerId } });
    return transport;
  };
  try {
    const first = await createClient("lifecycle-origin");
    const command = (client, name, request) => client.invokeCommand(name, { request });
    await command(first, "agent_runtime_config_set", {
      customModelProviders: [{
        providerId: "custom.lifecycle", name: "Lifecycle model",
        baseUrl: `http://127.0.0.1:${model.address().port}`, api: "openai-completions",
        models: [{ model: "lifecycle", displayName: "Lifecycle", contextTokens: "32k", maxOutputTokens: "4k", supportsVision: false }],
      }],
    });
    await command(first, "agent_runtime_config_set", { modelProviderId: "custom.lifecycle", modelApiKey: "test-only" });
    await command(first, "agent_runtime_config_set", { modelProviderId: "custom.lifecycle", model: "lifecycle" });
    const session = await command(first, "session/new", { operationId: "lifecycle-session", cwd, title: "Detached task" });
    const accepted = await command(first, "session/prompt", {
      operationId: "lifecycle-prompt", sessionId: session.id, message: "Keep working across client exits",
    });
    await eventually(() => requests.length, (count) => count === 1, "the initial model request");
    const page = await eventually(
      () => command(first, "transcript/page", { sessionId: session.id }),
      (result) => result.targetReached && result.page, "initial committed transcript",
    );
    await first.requestAppExit();
    // There are no connected clients here. This exceeds the Runtime idle timer.
    await delay(7_000);
    assert.equal(servers.length, 1);
    assert.equal(servers[0].exitCode, null, "an active run must retain the service without clients");
    const second = await createClient("lifecycle-observer");
    assert.equal(servers.length, 1, "reconnection must reuse the original Runtime process");
    const loaded = await command(second, "session/load", { sessionId: session.id });
    assert.equal(loaded.id, session.id);
    const readRun = async () => {
      const result = await command(second, "_centaeris/session/agent-runs", { sessionId: session.id, includeTerminal: true });
      return result.agentRuns.find((run) => run.agentRunId === accepted.agentRunId);
    };
    const running = await readRun();
    assert.ok(running && !terminal(running.status), "origin exit must not interrupt the same run");
    assert.equal(running.turnId, accepted.turnId);
    await command(second, "_centaeris/session/agent-runs/attach", { sessionId: session.id, viewerId: "lifecycle-observer" });
    const receipt = await command(second, "_centaeris/session/agent-runs/cancel", { sessionId: session.id, agentRunId: accepted.agentRunId, reason: "lifecycle_explicit_stop" });
    assert.equal(receipt.cancelAccepted, true);
    const stopped = await eventually(readRun, (run) => run && terminal(run.status), "explicit cancellation terminal fact");
    assert.equal(stopped.status, "cancelled");
    const patches = await eventually(
      () => command(second, "transcript/patches", {
        sessionId: session.id, projectionGeneration: page.projectionGeneration,
        afterSourceHighWater: page.page.sourceHighWater,
      }),
      (result) => result.targetReached, "committed transcript catch-up",
    );
    assert.ok(patches.patches.length > 0, "second client must catch up from the first client's cursor");
    assert.equal(requests.length, 1, "observation and cancellation must never replay the model request");
    await second.requestAppExit();
    await eventually(() => servers[0].exitCode, (code) => code !== null, "idle exit after the run terminates");
    assert.equal(servers[0].exitCode, 0);

    const shutdownClient = await createClient("lifecycle-shutdown");
    const shutdownRun = await command(shutdownClient, "session/prompt", {
      operationId: "lifecycle-shutdown-prompt", sessionId: session.id, message: "Hold until service shutdown",
    });
    await eventually(() => requests.length, (count) => count === 2, "the shutdown model request");
    const shutdownReceipt = await shutdownClient.invokeCommand("runtime/shutdown", {});
    assert.deepEqual(shutdownReceipt, { disposition: "requested" });
    await eventually(() => servers[1].exitCode, (code) => code !== null, "bounded graceful service shutdown");
    assert.equal(servers[1].exitCode, 0);
    const shutdownEvent = events.find((event) => event.eventName === "session/update"
      && event.payload?.agentRunId === shutdownRun.agentRunId
      && event.payload?.payload?.type === "session_event"
      && event.payload?.payload?.event?.type === "AgentRunInterrupted");
    assert.equal(shutdownEvent?.payload.payload.event.payload.reasonType, "shutdown", "service shutdown must not masquerade as user cancellation");
    // Mark the disconnected transport closed so test cleanup cannot reconnect it.
    await shutdownClient.requestAppExit();
    const recovered = await createClient("lifecycle-after-shutdown");
    const recoveredRuns = await command(recovered, "_centaeris/session/agent-runs", { sessionId: session.id, includeTerminal: true });
    assert.equal(recoveredRuns.agentRuns.find((run) => run.agentRunId === shutdownRun.agentRunId)?.status, "stopped");
    assert.equal(requests.length, 2, "restart must not replay an interrupted model call");
    await recovered.requestAppExit();
    await eventually(() => servers[2].exitCode, (code) => code !== null, "restarted idle service cleanup");
    assert.equal(servers[2].exitCode, 0);
  } finally {
    await Promise.allSettled(clients.map((client) => client.requestAppExit()));
    for (const response of heldResponses) response.destroy();
    model.closeAllConnections();
    await new Promise((resolve) => model.close(resolve));
    for (const server of servers) {
      if (server.exitCode === null && server.signalCode === null) {
        server.kill();
        await new Promise((resolve) => server.once("exit", resolve));
      }
    }
  }
};
