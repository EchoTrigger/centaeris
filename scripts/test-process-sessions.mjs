// Run after cargo build -p centaeris-runtime --bin centaeris-runtime.
// Uses an isolated profile and two real clients; never sends model requests.
import { createRuntimeHostTransport } from "../packages/desktop/src/runtimeHostTransport.mjs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const root = await fs.mkdtemp(
  path.join(os.tmpdir(), "centaeris-process-smoke-"),
);
const binary = path.join(
  repo,
  "target/debug",
  process.platform === "win32" ? "centaeris-runtime.exe" : "centaeris-runtime",
);
const environment = {
  ...process.env,
  CENTAERIS_DESKTOP_DATA_DIR: path.join(root, "profile"),
  CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
  CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
  CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
  CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
};
let server;
const connections = [];
function client() {
  const c = createRuntimeHostTransport({
    executablePath: binary,
    cwd: repo,
    environment,
    emitHostEvent: () => {},
    isAppReady: () => false,
    isQuitting: () => false,
    isSmokeRun: true,
    onRuntimeServerStarted: (p) => {
      server = p;
    },
  });
  connections.push(c);
  return c;
}
const call = (c, method, request) =>
  c.invokeCommand(method, method === "runtime/shutdown" ? {} : { request });
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function settle(c, target) {
  const until = Date.now() + 6000;
  while (Date.now() < until) {
    const s = await call(c, "process_session_get", target);
    if (s.state === "exited" && s.outputComplete && s.cleanupComplete) return s;
    await pause(30);
  }
  throw Error("process did not settle");
}
try {
  let desktop = client(),
    tui = client();
  await call(desktop, "initialize", {
    clientKind: "desktop",
    viewerId: "process-smoke-desktop",
  });
  await call(tui, "initialize", {
    clientKind: "tui",
    viewerId: "process-smoke-tui",
  });
  const session = await call(desktop, "session/new", {
    operationId: "process-smoke-session",
    cwd: root,
    title: "Process smoke",
  });
  const sessionId = session.id;
  assert.ok(sessionId);
  const { serviceInstanceId } = await call(desktop, "process_session_list", {
    sessionId,
  });
  assert.ok(serviceInstanceId);
  const request = {
    sessionId,
    serviceInstanceId,
    operationId: "process-smoke-start",
    program: "bash",
    args: ["-c", "printf ready; sleep 30"],
    timeoutMs: 60000,
  };
  const proc = await call(desktop, "process_session_start", request);
  const target = { sessionId, processSessionId: proc.processSessionId };
  assert.equal(
    (await call(tui, "process_session_start", request)).processSessionId,
    target.processSessionId,
  );
  await assert.rejects(
    call(tui, "process_session_start", {
      ...request,
      args: ["-c", "echo duplicate"],
    }),
    /conflict/,
  );
  const query = { ...target, cursor: "0", waitMs: 1000 };
  const first = await call(desktop, "process_session_read", query);
  const second = await call(tui, "process_session_read", query);
  assert.deepEqual(first.chunks, second.chunks);
  assert.equal(
    Buffer.concat(
      first.chunks.map((c) => Buffer.from(c.dataBase64, "base64")),
    ).toString(),
    "ready",
  );
  await assert.rejects(
    call(tui, "_centaeris/session/delete", { sessionId }),
    /active_processes/,
  );
  const pending = call(tui, "process_session_read", {
    ...query,
    cursor: first.nextCursor,
    waitMs: 3000,
  });
  const before = Date.now();
  await call(desktop, "session/list", {});
  assert.ok(Date.now() - before < 2000, "read held global Runtime lock");
  await pending;
  await desktop.requestAppExit();
  await tui.requestAppExit();
  await pause(6500);
  assert.equal(server.exitCode, null, "active process must keep Runtime alive");
  desktop = client();
  await call(desktop, "initialize", {
    clientKind: "desktop",
    viewerId: "process-smoke-reconnect",
  });
  assert.equal(
    (await call(desktop, "process_session_get", target)).state,
    "running",
  );
  await call(desktop, "process_session_stop", target);
  const stopped = await settle(desktop, target);
  assert.equal(stopped.terminationReason, "stopped");
  await call(desktop, "process_session_stop", target);
  await call(desktop, "_centaeris/session/delete", { sessionId });
  await assert.rejects(
    call(desktop, "process_session_get", target),
    /not_found_or_expired/,
  );
  for (let i = 0; i < 4; i++) {
    const race = await call(desktop, "session/new", {
      operationId: `race-session-${i}`,
      cwd: root,
      title: "Start/delete race",
    });
    const [start, deletion] = await Promise.allSettled([
      call(desktop, "process_session_start", {
        ...request,
        sessionId: race.id,
        operationId: `race-start-${i}`,
      }),
      call(desktop, "_centaeris/session/delete", { sessionId: race.id }),
    ]);
    if (start.status === "fulfilled") {
      assert.equal(
        deletion.status,
        "rejected",
        "active process escaped deletion fence",
      );
      const raceTarget = {
        sessionId: race.id,
        processSessionId: start.value.processSessionId,
      };
      await call(desktop, "process_session_stop", raceTarget);
      await settle(desktop, raceTarget);
      await call(desktop, "_centaeris/session/delete", { sessionId: race.id });
    } else {
      assert.equal(deletion.status, "fulfilled");
      assert.deepEqual(
        (await call(desktop, "process_session_list", { sessionId: race.id }))
          .processes,
        [],
      );
    }
  }
  const next = await call(desktop, "session/new", {
    operationId: "process-smoke-shutdown-session",
    cwd: root,
    title: "Shutdown smoke",
  });
  const shutdownProcess = await call(desktop, "process_session_start", {
    ...request,
    sessionId: next.id,
    operationId: "shutdown-child",
    args: [
      "-c",
      "(sleep 3; printf leak > shutdown-marker) & printf ready; wait",
    ],
  });
  assert.ok(
    (
      await call(desktop, "process_session_read", {
        sessionId: next.id,
        processSessionId: shutdownProcess.processSessionId,
        cursor: "0",
        waitMs: 1000,
      })
    ).chunks.length,
  );
  await call(desktop, "runtime/shutdown", {});
  await pause(4000);
  assert.notEqual(server.exitCode, null, "explicit shutdown must exit");
  await assert.rejects(fs.access(path.join(root, "shutdown-marker")));
  console.log(
    "PASS: real Desktop/TUI transport, independent output, duplicate start, delete fence, nonblocking read, disconnected survival, scoped stop, shutdown tree cleanup",
  );
} finally {
  for (const c of connections) await c.requestAppExit();
  if (server?.exitCode === null) {
    server.kill();
    await pause(100);
  }
  // Retain isolated evidence on failure/success; no user data is modified.
  console.log(`Isolated profile: ${root}`);
}
