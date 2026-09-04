import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { DatabaseSync } from "node:sqlite";
import { createRuntimeHostTransport } from "../src/runtimeHostTransport.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");
const defaultRuntimeExe = path.join(
  repoRoot,
  "target",
  "release",
  "centaeris-runtime.exe",
);
const runtimeExe = process.env.CENTAERIS_ELECTRON_SMOKE_RUNTIME_EXE || defaultRuntimeExe;
const REQUEST_TIMEOUT_MS = 30_000;
const PROCESS_TIMEOUT_MS = 30_000;
const RUNTIME_SERVER_IDLE_CLEANUP_MS = 7_000;

const requiredResponses = new Map();
const events = [];
let nextRequestId = 1;
let stderrTail = "";

const fail = (message) => {
  throw new Error(`${message}${stderrTail ? `\nRust stderr:\n${stderrTail}` : ""}`);
};

const appendStderr = (chunk) => {
  stderrTail = `${stderrTail}${chunk.toString()}`.slice(-8000);
};

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const runtimeServerEndpoint = (environment) =>
  new Promise((resolve, reject) => {
    const probe = spawn(runtimeExe, ["--runtime-server-endpoint"], {
      cwd: repoRoot,
      env: environment,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    probe.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    probe.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    probe.once("error", reject);
    probe.once("exit", (code) => {
      if (code !== 0) {
        reject(new Error(`runtime endpoint discovery failed: ${stderr.trim()}`));
        return;
      }
      try {
        const endpoint = String(JSON.parse(stdout).endpoint ?? "").trim();
        if (!endpoint) throw new Error("descriptor is missing endpoint");
        resolve(endpoint);
      } catch (error) {
        reject(new Error(`invalid runtime endpoint descriptor: ${error.message}`));
      }
    });
  });

const connectRuntimeServer = (endpoint) =>
  new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint);
    const onError = (error) => {
      socket.off("connect", onConnect);
      reject(error);
    };
    const onConnect = () => {
      socket.off("error", onError);
      resolve(socket);
    };
    socket.once("error", onError);
    socket.once("connect", onConnect);
  });

const waitForRuntimeServer = async (endpoint) => {
  let lastError = new Error("runtime server did not accept a connection");
  for (let attempt = 0; attempt < 50; attempt += 1) {
    try {
      return await connectRuntimeServer(endpoint);
    } catch (error) {
      lastError = error;
      await delay(100);
    }
  }
  throw lastError;
};

const removeTempRoot = async (root) => {
  let lastError;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      await fs.rm(root, { recursive: true, force: true });
      return;
    } catch (error) {
      lastError = error;
      if (
        process.platform !== "win32" ||
        !["EBUSY", "EPERM", "ENOTEMPTY"].includes(error?.code)
      ) {
        throw error;
      }
      await delay(100);
    }
  }
  throw lastError;
};

const assertRecord = (value, label) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label} must be an object`);
  }
};

const assertArray = (value, label) => {
  if (!Array.isArray(value)) {
    fail(`${label} must be an array`);
  }
};

const assertOkResponse = (value, label) => {
  assertRecord(value, label);
  if (value.ok !== true) {
    fail(`${label} must acknowledge ok=true`);
  }
};

const assertHostTransportIsolatesInvalidLiveText = async (tempRoot) => {
  const isolationRoot = path.join(tempRoot, "live-text-isolation");
  const liveTextRoot = path.join(isolationRoot, "runtime", "live-text");
  await fs.mkdir(liveTextRoot, { recursive: true });
  await fs.writeFile(path.join(liveTextRoot, "banana.live"), "banana", "utf8");
  const environment = {
    ...process.env,
    CENTAERIS_DESKTOP_DATA_DIR: isolationRoot,
    CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
    CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
  };
  let startedServer = null;
  const transport = createRuntimeHostTransport({
    executablePath: runtimeExe,
    cwd: repoRoot,
    environment,
    emitHostEvent: () => {},
    isAppReady: () => false,
    isQuitting: () => false,
    isSmokeRun: true,
    onRuntimeServerStarted: (child) => { startedServer = child; },
  });
  try {
    const descriptor = await transport.invokeCommand("initialize", {
      request: { clientKind: "desktop", viewerId: "desktop-live-text-isolation-smoke" },
    });
    if (descriptor.status !== "ok") {
      fail("Runtime Host transport did not initialize after isolating invalid live text");
    }
    const sessions = await transport.invokeCommand("session/list", { request: {} });
    if (!Array.isArray(sessions)) {
      fail("Runtime Host transport did not remain usable after live text isolation");
    }
  } finally {
    await transport.requestAppExit();
    await delay(RUNTIME_SERVER_IDLE_CLEANUP_MS);
    if (startedServer && startedServer.exitCode === null && startedServer.signalCode === null) {
      startedServer.kill();
      await waitForExit(startedServer);
      fail("Runtime Server remained alive after live text isolation transport exit");
    }
  }
};

const assertHostTransportReconnectsAfterRuntimeExit = async (tempRoot) => {
  const reconnectRoot = path.join(tempRoot, "transport-reconnect");
  const workspaceRoot = path.join(reconnectRoot, "workspace");
  await fs.mkdir(path.join(reconnectRoot, "skills", "system"), { recursive: true });
  await fs.mkdir(workspaceRoot, { recursive: true });
  const environment = {
    ...process.env,
    CENTAERIS_DESKTOP_DATA_DIR: reconnectRoot,
    CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
    CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
    OPENAI_API_KEY: "",
    DEEPSEEK_API_KEY: "",
    KIMI_API_KEY: "",
    CENTAERIS_MODEL_API_KEY: "",
  };
  const endpoint = await runtimeServerEndpoint(environment);
  const server = spawn(runtimeExe, ["--runtime-server"], {
    cwd: repoRoot,
    env: environment,
    stdio: ["ignore", "ignore", "pipe"],
    windowsHide: true,
  });
  server.stderr.on("data", appendStderr);
  const connection = await waitForRuntimeServer(endpoint);
  connection.destroy();
  let markDisconnected;
  let reconnectedServer = null;
  const disconnected = new Promise((resolve) => { markDisconnected = resolve; });
  const transport = createRuntimeHostTransport({
    executablePath: runtimeExe,
    cwd: repoRoot,
    environment,
    emitHostEvent: (eventName, payload) => {
      if (eventName === "centaeris/runtime-host-error"
        && String(payload?.message).includes("connection closed")) {
        markDisconnected();
      }
    },
    isAppReady: () => false,
    isQuitting: () => false,
    isSmokeRun: true,
    onRuntimeServerStarted: (child) => { reconnectedServer = child; },
  });
  try {
    await transport.invokeCommand("initialize", {
      request: { clientKind: "desktop", viewerId: "desktop-reconnect-smoke" },
    });
    await transport.invokeCommand("workspace_activate", {
      request: { root: workspaceRoot },
    });
    const created = await transport.invokeCommand("session/new", {
      request: { title: "transport reconnect smoke", cwd: workspaceRoot },
    });
    server.kill();
    await waitForExit(server);
    let disconnectTimeout;
    try {
      await Promise.race([
        disconnected,
        new Promise((_, reject) => {
          disconnectTimeout = setTimeout(
            () => reject(new Error("Runtime Host transport did not observe server exit")),
            REQUEST_TIMEOUT_MS,
          );
        }),
      ]);
    } finally {
      clearTimeout(disconnectTimeout);
    }
    const loaded = await transport.invokeCommand("session/load", {
      request: { sessionId: created.id },
    });
    if (loaded.id !== created.id) {
      fail("Runtime Host transport did not continue the existing session after reconnect");
    }
  } finally {
    await transport.requestAppExit();
    await delay(RUNTIME_SERVER_IDLE_CLEANUP_MS);
    if (reconnectedServer
      && reconnectedServer.exitCode === null && reconnectedServer.signalCode === null) {
      reconnectedServer.kill();
      await waitForExit(reconnectedServer);
      fail("Runtime Server remained alive after reconnect transport exit");
    }
    if (server.exitCode === null && server.signalCode === null) {
      server.kill();
      await waitForExit(server);
    }
  }
};

const assertHostTransportReplacesMismatchedRuntime = async (tempRoot) => {
  const replacementRoot = path.join(tempRoot, "transport-build-replacement");
  const staleRuntimeExe = path.join(replacementRoot, "stale-runtime.exe");
  await fs.mkdir(path.join(replacementRoot, "skills", "system"), { recursive: true });
  await fs.copyFile(runtimeExe, staleRuntimeExe);
  await fs.appendFile(staleRuntimeExe, "stale-build-overlay");
  const environment = {
    ...process.env,
    CENTAERIS_DESKTOP_DATA_DIR: replacementRoot,
    CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
    CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
    OPENAI_API_KEY: "",
    DEEPSEEK_API_KEY: "",
    KIMI_API_KEY: "",
    CENTAERIS_MODEL_API_KEY: "",
  };
  const endpoint = await runtimeServerEndpoint(environment);
  const staleServer = spawn(staleRuntimeExe, ["--runtime-server"], {
    cwd: repoRoot,
    env: environment,
    stdio: ["ignore", "ignore", "pipe"],
    windowsHide: true,
  });
  staleServer.stderr.on("data", appendStderr);
  const connection = await waitForRuntimeServer(endpoint);
  connection.destroy();
  let replacementServer = null;
  const transport = createRuntimeHostTransport({
    executablePath: runtimeExe,
    cwd: repoRoot,
    environment,
    emitHostEvent: () => {},
    isAppReady: () => false,
    isQuitting: () => false,
    isSmokeRun: true,
    onRuntimeServerStarted: (child) => { replacementServer = child; },
  });
  try {
    const descriptor = await transport.invokeCommand("initialize", {
      request: { clientKind: "desktop", viewerId: "desktop-build-replacement-smoke" },
    });
    if (!/^sha256:[0-9a-f]{64}$/.test(descriptor.buildId)) {
      fail("Runtime Host transport replacement returned an invalid buildId");
    }
    await transport.invokeCommand("session/list", { request: {} });
    const staleExit = await waitForExit(staleServer);
    if (staleExit.code !== 0) {
      fail(`stale Runtime did not exit cleanly: code=${staleExit.code} signal=${staleExit.signal}`);
    }
  } finally {
    await transport.requestAppExit();
    await delay(RUNTIME_SERVER_IDLE_CLEANUP_MS);
    if (replacementServer
      && replacementServer.exitCode === null && replacementServer.signalCode === null) {
      replacementServer.kill();
      await waitForExit(replacementServer);
      fail("replacement Runtime Server remained alive after transport exit");
    }
    if (staleServer.exitCode === null && staleServer.signalCode === null) {
      staleServer.kill();
      await waitForExit(staleServer);
    }
  }
};

const assertUnsupportedRuntimeStoreFailsBeforeListening = async (tempRoot, environment) => {
  const failureRoot = path.join(tempRoot, "unsupported-runtime-store");
  const databasePath = path.join(failureRoot, "runtime", "runtime.sqlite3");
  await fs.mkdir(path.dirname(databasePath), { recursive: true });
  const database = new DatabaseSync(databasePath);
  try {
    database.exec(`
      PRAGMA journal_mode = WAL;
      CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL);
      INSERT INTO schema_migrations VALUES(14, 1);
    `);
  } finally {
    database.close();
  }
  const originalBytes = await fs.readFile(databasePath);
  const failureEnvironment = {
    ...environment,
    CENTAERIS_DESKTOP_DATA_DIR: failureRoot,
    CENTAERIS_CONFIG_PATH: path.join(failureRoot, "config.toml"),
    CENTAERIS_AGENT_RUNTIME_DB_PATH: databasePath,
    CENTAERIS_MESSAGE_LOG_SESSIONS_DIR: path.join(failureRoot, "sessions"),
  };
  const endpoint = await runtimeServerEndpoint(failureEnvironment);
  // A second attempt must reach schema validation again, proving the singleton lock was released.
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const child = spawn(runtimeExe, ["--runtime-server"], {
      cwd: repoRoot,
      env: failureEnvironment,
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
    });
    let stderr = "";
    child.stderr.on("data", (chunk) => { stderr += chunk.toString(); });
    const exit = await waitForExit(child);
    if (exit.code === 0 || exit.signal !== null
      || !stderr.includes("runtime_server_store_init_failed")
      || !stderr.includes("runtime sqlite refuses schema downgrade: store version 14, runtime version 1")) {
      fail(`unsupported Runtime store did not fail clearly at startup: ${stderr}`);
    }
    let unexpectedConnection = null;
    try {
      unexpectedConnection = await connectRuntimeServer(endpoint);
    } catch (error) {
      if (!["ENOENT", "ECONNREFUSED"].includes(error.code)) throw error;
    }
    if (unexpectedConnection) {
      unexpectedConnection.destroy();
      fail("unsupported Runtime store left a listening endpoint");
    }
    if (!(await fs.readFile(databasePath)).equals(originalBytes)) {
      fail("unsupported Runtime store changed the existing database bytes");
    }
  }
};

const pathExists = async (targetPath) => {
  try {
    await fs.access(targetPath);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
};

const normalizeComparablePath = (value) => {
  const raw = String(value).replace(/^\\\\\?\\/, "");
  const normalized = path.normalize(raw);
  return process.platform === "win32" ? normalized.toLowerCase() : normalized;
};

const pathsEqual = (left, right) =>
  normalizeComparablePath(left) === normalizeComparablePath(right);

const runtimeErrorMessage = (error) => {
  if (!error) {
    return "unknown error";
  }
  if (typeof error === "string") {
    return error;
  }
  if (typeof error.message === "string") {
    return error.message;
  }
  return JSON.stringify(error);
};

const invoke = (child, command, payload = {}) => {
  const id = `smoke-${nextRequestId++}`;
  const request = {
    jsonrpc: "2.0",
    id,
    method: command,
    params: payload,
  };
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      requiredResponses.delete(id);
      reject(new Error(`timed out waiting for ${command}`));
    }, REQUEST_TIMEOUT_MS);
    requiredResponses.set(id, {
      command,
      resolve: (value) => {
        clearTimeout(timeout);
        resolve(value);
      },
      reject: (error) => {
        clearTimeout(timeout);
        reject(error);
      },
    });
    const line = `${JSON.stringify(request)}\n`;
    const canContinue = child.stdin.write(line);
    if (!canContinue) {
      child.stdin.once("drain", () => {});
    }
  });
};

const handleLine = (line) => {
  if (!line.trim()) {
    return;
  }
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    fail(`invalid JSONL from Runtime Host: ${error.message}`);
  }
  if (message?.jsonrpc !== "2.0") {
    fail("Runtime Host message missing JSON-RPC 2.0 envelope");
  }
  const hasOwn = (key) => Object.prototype.hasOwnProperty.call(message, key);
  if (typeof message.method === "string" && !hasOwn("id")) {
    events.push({
      kind: "event",
      eventName: message.method,
      payload: message.params ?? {},
    });
    return;
  }
  const hasResult = hasOwn("result");
  const hasError = hasOwn("error");
  if (!hasOwn("id") || hasResult === hasError) {
    fail("Runtime Host message must be a JSON-RPC response or notification");
  }
  const responseId = String(message.id);
  const pending = requiredResponses.get(responseId);
  if (!pending) {
    fail(`Runtime Host response has no pending request: ${responseId}`);
  }
  requiredResponses.delete(responseId);
  if (hasResult) {
    pending.resolve(message.result);
    return;
  }
  const error = new Error(
    `${pending.command} failed: ${runtimeErrorMessage(message.error)}`,
  );
  if (message.error && typeof message.error === "object") {
    error.code =
      typeof message.error.data?.code === "string"
        ? message.error.data.code
        : message.error.code;
    error.payload = message.error;
  }
  pending.reject(error);
};

const invokeExpectError = async (child, command, payload, expectedCode) => {
  try {
    await invoke(child, command, payload);
  } catch (error) {
    if (error.code !== expectedCode) {
      fail(
        `${command} failed with unexpected error code ${
          error.code ?? "unknown"
        }, expected ${expectedCode}: ${error.message}`,
      );
    }
    return;
  }
  fail(`${command} unexpectedly succeeded`);
};

const writeRawFrameExpectError = async (child, id, frame, expectedCode) => {
  try {
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        requiredResponses.delete(id);
        reject(new Error(`timed out waiting for raw frame ${id}`));
      }, REQUEST_TIMEOUT_MS);
      requiredResponses.set(id, {
        command: `raw:${id}`,
        resolve: (value) => {
          clearTimeout(timeout);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timeout);
          reject(error);
        },
      });
      const canContinue = child.stdin.write(`${JSON.stringify(frame)}\n`);
      if (!canContinue) {
        child.stdin.once("drain", () => {});
      }
    });
  } catch (error) {
    if (error.code !== expectedCode) {
      fail(
        `raw frame ${id} failed with unexpected error code ${
          error.code ?? "unknown"
        }, expected ${expectedCode}: ${error.message}`,
      );
    }
    return;
  }
  fail(`raw frame ${id} unexpectedly succeeded`);
};

const waitForEvent = async (predicate, label, timeoutMs = REQUEST_TIMEOUT_MS) => {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const event = events.find(predicate);
    if (event) {
      return event;
    }
    await delay(50);
  }
  fail(`timed out waiting for event: ${label}`);
};

const waitForAgentRunDoneOrError = async (
  agentRunId,
  label,
  timeoutMs = REQUEST_TIMEOUT_MS,
) => {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const event = events.find(
      (item) => {
        if (
          item.eventName !== "session/update" ||
          item.payload?.agentRunId !== agentRunId
        ) {
          return false;
        }
        const payload = item.payload?.payload;
        if (
          payload?.type === "runtime_event" &&
          payload?.event?.type === "RuntimeError"
        ) {
          return true;
        }
        if (payload?.type !== "session_event") {
          return false;
        }
        return ["AgentRunCompleted", "AgentRunFailed", "AgentRunInterrupted"].includes(
          String(payload.event?.type ?? "").trim(),
        );
      },
    );
    const payload = event?.payload?.payload;
    if (payload?.type === "runtime_event" && payload?.event?.type === "RuntimeError") {
      fail(`${label} failed: ${payload.event.payload?.message ?? "unknown error"}`);
    }
    if (
      payload?.type === "session_event" &&
      payload?.event?.type === "AgentRunFailed"
    ) {
      fail(`${label} failed: ${payload.event?.payload?.message ?? "unknown error"}`);
    }
    if (event) {
      return event;
    }
    await delay(50);
  }
  fail(`timed out waiting for task completion: ${label}`);
};

const startOpenAiCompatibleMockServer = async () => {
  const requests = [];
  let heldStreamingResponse = null;
  const server = http.createServer((request, response) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => {
      body += chunk;
    });
    request.on("end", () => {
      requests.push({
        method: request.method,
        url: request.url,
        authorization: request.headers.authorization,
        body,
      });
      const payload = JSON.parse(body);
      if (payload.stream !== true) {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({
          id: "chatcmpl-centaeris-smoke",
          choices: [{
            index: 0,
            message: { role: "assistant", content: "OK" },
            finish_reason: "stop",
          }],
          usage: { prompt_tokens: 3, completion_tokens: 1, total_tokens: 4 },
        }));
        return;
      }
      response.writeHead(200, {
        "content-type": "text/event-stream; charset=utf-8",
        "cache-control": "no-cache",
      });
      response.write(
        `data: ${JSON.stringify({
          id: "chatcmpl-centaeris-smoke",
          choices: [
            {
              index: 0,
              delta: { role: "assistant", content: "Hello from smoke" },
              finish_reason: null,
            },
          ],
        })}\n\n`,
      );
      if (body.includes("runtime crash recovery smoke")) {
        heldStreamingResponse = response;
        return;
      }
      response.write(
        `data: ${JSON.stringify({
          id: "chatcmpl-centaeris-smoke",
          choices: [
            {
              index: 0,
              message: {
                role: "assistant",
                content: "Hello from smoke",
              },
              delta: {},
              finish_reason: "stop",
            },
          ],
          usage: {
            prompt_tokens: 3,
            completion_tokens: 3,
            total_tokens: 6,
          },
        })}\n\n`,
      );
      response.write("data: [DONE]\n\n");
      response.end();
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("mock server did not bind to a TCP address");
  }
  return {
    baseUrl: `http://127.0.0.1:${address.port}`,
    requests,
    close: () => new Promise((resolve) => {
      heldStreamingResponse?.destroy();
      server.close(resolve);
    }),
  };
};

const waitForExit = async (child) => {
  if (child.exitCode !== null || child.signalCode !== null) {
    return { code: child.exitCode, signal: child.signalCode };
  }
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`Runtime Host did not exit within ${PROCESS_TIMEOUT_MS}ms`));
    }, PROCESS_TIMEOUT_MS);
    child.once("exit", (code, signal) => {
      clearTimeout(timeout);
      resolve({ code, signal });
    });
  });
};

const slowCaptureRequest = () => {
  if (process.platform === "win32") {
    return {
      program: "cmd",
      args: ["/C", "ping -n 3 127.0.0.1 >nul && echo slow-capture-done"],
      timeoutMs: 10_000,
      maxOutputChars: 400,
    };
  }
  return {
    program: "sh",
    args: ["-lc", "sleep 2; echo slow-capture-done"],
    timeoutMs: 10_000,
    maxOutputChars: 400,
  };
};

const main = async () => {
  await fs.access(runtimeExe);
  const tempRoot = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-runtime-smoke-"));
  await fs.mkdir(path.join(tempRoot, "skills", "system"), { recursive: true });
  const runtimeEnvironment = {
    ...process.env,
    CENTAERIS_DESKTOP_DATA_DIR: tempRoot,
    CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
    CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
    CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
    OPENAI_API_KEY: "",
    DEEPSEEK_API_KEY: "",
    KIMI_API_KEY: "",
    CENTAERIS_MODEL_API_KEY: "",
  };
  const endpoint = await runtimeServerEndpoint(runtimeEnvironment);
  let server = null;
  let socket = null;
  let lines = null;
  const child = { stdin: null };
  const initializePayload = {
    request: { clientKind: "desktop", viewerId: "desktop-smoke" },
  };
  const startRuntimeServer = async () => {
    server = spawn(runtimeExe, ["--runtime-server"], {
      cwd: repoRoot,
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
      env: runtimeEnvironment,
    });
    server.stderr.on("data", appendStderr);
    server.once("error", (error) => fail(`failed to start Runtime Server: ${error.message}`));
    socket = await waitForRuntimeServer(endpoint);
    child.stdin = socket;
    lines = readline.createInterface({ input: socket, crlfDelay: Infinity });
    lines.on("line", handleLine);
  };
  const restartRuntimeServer = async () => {
    lines?.close();
    lines = null;
    socket?.destroy();
    socket = null;
    child.stdin = null;
    server.kill();
    await waitForExit(server);
    await startRuntimeServer();
    await invoke(child, "initialize", initializePayload);
  };
  await startRuntimeServer();

  try {
    const descriptor = await invoke(child, "initialize", initializePayload);
    assertRecord(descriptor, "initialize result");
    if (
      descriptor.status !== "ok" ||
      descriptor.runtime !== "centaeris-runtime" ||
      descriptor.protocol !== "centaeris.runtime" ||
      descriptor.protocolVersion !== 1 ||
      descriptor.coreProtocolVersion !== "1.0.0" ||
      !/^sha256:[0-9a-f]{64}$/.test(descriptor.buildId) ||
      typeof descriptor.profileId !== "string" || !descriptor.profileId ||
      typeof descriptor.storeId !== "string" || !descriptor.storeId ||
      !Number.isInteger(descriptor.storeSchemaVersion) || descriptor.storeSchemaVersion < 1 ||
      !Number.isInteger(descriptor.layoutSchemaVersion) || descriptor.layoutSchemaVersion < 1
    ) {
      fail("initialize returned an unexpected runtime protocol response");
    }
    if (
      !descriptor.capabilities?.includes("json_rpc_2_over_jsonl") ||
      !descriptor.events?.includes("runtime/config-changed") ||
      !["runtime_event", "session_event"].every((projection) =>
        descriptor.projections?.includes(projection)
      )
    ) {
      fail("initialize returned an incomplete Centaeris Runtime Protocol descriptor");
    }
    if (await pathExists(path.join(tempRoot, "workspaces"))) {
      fail("initialize must not create a managed default workspace directory");
    }
    let missingWorkingDirectoryError = null;
    try {
      await invoke(child, "session/new", {
        request: { title: "missing workspace must fail" },
      });
    } catch (error) {
      missingWorkingDirectoryError = error;
    }
    if (!missingWorkingDirectoryError) {
      fail("session/new without cwd must fail");
    }
    if (!runtimeErrorMessage(missingWorkingDirectoryError).includes("cwd")) {
      fail(
        `session/new missing cwd returned the wrong error: ${runtimeErrorMessage(missingWorkingDirectoryError)}`,
      );
    }

    const slowCapture = invoke(child, "process_capture", slowCaptureRequest());
    const initializeDuringSlowCapture = await invoke(
      child,
      "initialize",
      initializePayload,
    );
    assertRecord(initializeDuringSlowCapture, "initialize during slow process_capture result");
    if (initializeDuringSlowCapture.status !== "ok") {
      fail("initialize should return while process_capture is still running");
    }
    const slowCaptureResult = await slowCapture;
    assertRecord(slowCaptureResult, "slow process_capture result");
    if (!slowCaptureResult.stdout.includes("slow-capture-done")) {
      fail("slow process_capture did not complete after concurrent initialize check");
    }

    const workspaceRootRaw = path.join(tempRoot, "workspace");
    await fs.mkdir(workspaceRootRaw, { recursive: true });
    const workspaceRoot = await fs.realpath(workspaceRootRaw);
    const sessions = await invoke(child, "session/list", {
      request: {},
    });
    assertArray(sessions, "session/list result");
    const sessionDiagnostics = await invoke(child, "_centaeris/session/diagnostics", {
      request: {},
    });
    assertArray(sessionDiagnostics, "_centaeris/session/diagnostics result");
    await fs.writeFile(
      path.join(workspaceRoot, "sample.txt"),
      "Centaeris Local Runtime Host workspace smoke\n",
      "utf8",
    );
    const previewPdfPath = path.join(tempRoot, "sample-preview.pdf");
    await fs.writeFile(
      previewPdfPath,
      Buffer.from("%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF\n"),
    );
    const outsidePreviewPath = path.join(tempRoot, "outside-preview.txt");
    await fs.writeFile(
      outsidePreviewPath,
      "Centaeris Local Runtime Host desktop preview smoke\n",
      "utf8",
    );

    const activatedWorkspace = await invoke(child, "workspace_activate", {
      request: { root: workspaceRoot },
    });
    assertRecord(activatedWorkspace, "workspace_activate result");
    assertArray(activatedWorkspace.workspaces, "workspace_activate.workspaces");
    if (!pathsEqual(activatedWorkspace.activeWorkspaceRoot, workspaceRoot)) {
      fail(
        `workspace_activate did not persist the requested active root: expected=${workspaceRoot} actual=${activatedWorkspace.activeWorkspaceRoot}`,
      );
    }

    const workspace = await invoke(child, "workspace_get", {});
    assertRecord(workspace, "workspace_get result");
    if (!pathsEqual(workspace.activeWorkspaceRoot, workspaceRoot)) {
      fail(
        `workspace_get did not return the active workspace root: expected=${workspaceRoot} actual=${workspace.activeWorkspaceRoot}`,
      );
    }

    const fileTree = await invoke(child, "workspace_file_tree", {
      request: { workspaceRoot, maxDepth: 2 },
    });
    assertRecord(fileTree, "workspace_file_tree result");
    assertArray(fileTree.entries, "workspace_file_tree.entries");
    if (!fileTree.entries.some((entry) => entry.name === "sample.txt")) {
      fail("workspace_file_tree did not include sample.txt");
    }

    const filePreview = await invoke(child, "workspace_read_file", {
      request: { workspaceRoot, path: "sample.txt" },
    });
    assertRecord(filePreview, "workspace_read_file result");
    if (!filePreview.content.includes("workspace smoke")) {
      fail("workspace_read_file returned unexpected file content");
    }

    const desktopAbsolutePreview = await invoke(child, "desktop_file_preview_read", {
      request: { path: outsidePreviewPath },
    });
    assertRecord(desktopAbsolutePreview, "desktop_file_preview_read absolute result");
    if (!desktopAbsolutePreview.content.includes("desktop preview smoke")) {
      fail("desktop_file_preview_read did not read absolute non-workspace text");
    }

    const desktopRelativePreview = await invoke(child, "desktop_file_preview_read", {
      request: { workspaceRoot, path: "sample.txt" },
    });
    assertRecord(desktopRelativePreview, "desktop_file_preview_read relative result");
    if (!desktopRelativePreview.content.includes("workspace smoke")) {
      fail("desktop_file_preview_read did not read workspace-relative text");
    }

    const desktopPdfPreview = await invoke(child, "desktop_file_preview_read", {
      request: { path: previewPdfPath },
    });
    assertRecord(desktopPdfPreview, "desktop_file_preview_read PDF result");
    if (
      desktopPdfPreview.contentKind !== "pdf" ||
      desktopPdfPreview.mimeType !== "application/pdf" ||
      !String(desktopPdfPreview.dataUrl || "").startsWith("data:application/pdf;base64,")
    ) {
      fail("desktop_file_preview_read did not return PDF preview metadata");
    }

    const workspaceSession = await invoke(child, "session/new", {
      request: {
        title: "workspace smoke session",
        cwd: workspaceRoot,
      },
    });
    assertRecord(workspaceSession, "workspace session/new result");
    if (
      !pathsEqual(workspaceSession.cwd, workspaceRoot)
    ) {
      fail("workspace session/new did not persist cwd");
    }
    const sharedSessions = await invoke(child, "session/list", {
      request: {},
    });
    assertArray(sharedSessions, "workspace session/list result");
    if (!sharedSessions.some((session) => session.id === workspaceSession.id)) {
      fail("workspace session/list did not return session/new result");
    }
    const loadedSession = await invoke(child, "session/load", {
      request: { sessionId: workspaceSession.id },
    });
    assertRecord(loadedSession, "workspace session/load result");
    if (loadedSession.id !== workspaceSession.id || !Array.isArray(loadedSession.messages)) {
      fail("workspace session/load did not return the created session");
    }
    const activatedSession = await invoke(child, "_centaeris/session/activate", {
      request: { sessionId: workspaceSession.id, selectedAtMs: Date.now() },
    });
    assertRecord(activatedSession, "workspace session/activate result");
    if (activatedSession.id !== workspaceSession.id) {
      fail("workspace session/activate returned a different session");
    }
    const disposableSession = await invoke(child, "session/new", {
      request: {
        title: "delete smoke session",
        cwd: workspaceRoot,
      },
    });
    const deletedSessions = await invoke(child, "_centaeris/session/delete", {
      request: { sessionId: disposableSession.id },
    });
    if (deletedSessions.deletedSessionId !== disposableSession.id) {
      fail("session delete returned a mismatched deletedSessionId");
    }
    const sessionsAfterDelete = await invoke(child, "session/list", {
      request: {},
    });
    if (sessionsAfterDelete.some((session) => session.id === disposableSession.id)) {
      fail("deleted session remained in session/list");
    }
    const sessionProjection = await invoke(child, "_centaeris/session/project", {
      request: { sessionId: workspaceSession.id },
    });
    assertRecord(sessionProjection, "workspace session/project result");
    const sessionFileTree = await invoke(child, "workspace_file_tree", {
      request: {
        sessionId: workspaceSession.id,
        workspaceRoot,
        maxDepth: 2,
      },
    });
    assertRecord(sessionFileTree, "workspace_file_tree with sessionId result");
    assertArray(sessionFileTree.entries, "workspace_file_tree with sessionId entries");
    if (!sessionFileTree.entries.some((entry) => entry.name === "sample.txt")) {
      fail("workspace_file_tree with sessionId did not include sample.txt");
    }

    await invokeExpectError(
      child,
      "workspace_open_folder",
      {},
      "workspace_failed",
    );

    const config = await invoke(child, "agent_runtime_config_get", {
      request: {},
    });
    assertRecord(config, "agent_runtime_config_get result");
    if (config.executionHost !== "localUser") {
      fail("agent_runtime_config_get must use the local-user execution host");
    }
    if (config.modelProviderId || config.model || config.selectableModels?.length) {
      fail("agent_runtime_config_get must not inject an implicit model");
    }

    const skillSources = await invoke(child, "skill/source/list", { request: {} });
    assertArray(skillSources.sources, "skill/source/list sources");
    if (
      !skillSources.sources.some(
        (source) => source.sourceId === "centaeris-system-skills" && source.scope === "system",
      )
    ) {
      fail("skill/source/list did not include the fixed System Skill source");
    }
    const skillCatalog = await invoke(child, "skill/catalog", {
      request: { cwd: workspaceRoot },
    });
    assertArray(skillCatalog.skills, "skill/catalog skills");
    if (skillCatalog.skills.length !== 0) {
      fail("clean public runtime must not inject concrete System Skills");
    }

    const mcpCatalog = await invoke(child, "mcp/catalog", { request: {} });
    assertRecord(mcpCatalog, "mcp/catalog result");
    if (mcpCatalog.schema !== "native.mcp.catalog.v1") {
      fail("mcp/catalog returned an unsupported schema");
    }
    assertArray(mcpCatalog.servers, "mcp/catalog servers");
    if (mcpCatalog.servers.length !== 0) {
      fail("clean public runtime must not inject concrete MCP servers");
    }

    const savedConfig = await invoke(child, "agent_runtime_config_set", {
      request: { toolParallelism: 9 },
    });
    assertRecord(savedConfig, "agent_runtime_config_set result");
    if (savedConfig.toolParallelism !== 9) {
      fail("agent_runtime_config_set did not persist toolParallelism=9");
    }
    const configChanged = await waitForEvent(
      (event) => event.eventName === "runtime/config-changed",
      "agent_runtime_config_set notification",
    );
    if (
      !configChanged.payload ||
      Array.isArray(configChanged.payload) ||
      Object.keys(configChanged.payload).length !== 0
    ) {
      fail("runtime/config-changed payload must be an empty object");
    }
    const savedBashConfig = await invoke(child, "agent_runtime_config_set", {
      request: { bashPath: "" },
    });
    assertRecord(savedBashConfig, "agent_runtime_config_set Bash result");
    if (savedBashConfig.executionHost !== "localUser" || savedBashConfig.bashPath) {
      fail("agent_runtime_config_set did not retain automatic platform Bash resolution");
    }

    const contextUsage = await invoke(child, "agent_context_usage_get", {
      request: { sessionId: workspaceSession.id },
    });
    assertRecord(contextUsage, "agent_context_usage_get result");

    const agentState = await invoke(child, "agent_state_get", {
      request: { sessionId: workspaceSession.id, includeRuntimeState: true },
    });
    assertRecord(agentState, "agent_state_get result");

    const tasks = await invoke(child, "_centaeris/session/agent-runs", {});
    assertRecord(tasks, "_centaeris/session/agent-runs result");
    assertArray(tasks.agentRuns, "_centaeris/session/agent-runs.agentRuns");

    const runtimeJobs = await invoke(child, "agent_runtime_job_list", {
      request: { limit: 5 },
    });
    assertRecord(runtimeJobs, "agent_runtime_job_list result");
    assertArray(runtimeJobs.jobs, "agent_runtime_job_list.jobs");

    const deadLetters = await invoke(child, "agent_dead_letter_list", {
      request: { limit: 5 },
    });
    assertRecord(deadLetters, "agent_dead_letter_list result");
    assertArray(deadLetters.deadLetters, "agent_dead_letter_list.deadLetters");

    const runtimeGarbage = await invoke(child, "agent_runtime_garbage_collect", {
      request: { dryRun: true, documentCacheGraceMs: 1000 },
    });
    assertRecord(runtimeGarbage, "agent_runtime_garbage_collect result");
    if (
      runtimeGarbage.schema !== "runtime_garbage_collect_v1" ||
      runtimeGarbage.dryRun !== true
    ) {
      fail("agent_runtime_garbage_collect returned unexpected schema or dryRun");
    }
    assertArray(runtimeGarbage.items, "agent_runtime_garbage_collect.items");

    const projection = await invoke(child, "transcript/project", {
      request: { streamItems: [] },
    });
    assertRecord(projection, "transcript/project result");
    assertArray(projection.lines, "transcript/project.lines");

    const sidecars = await invoke(child, "sidecar_list", {});
    assertRecord(sidecars, "sidecar_list result");
    assertArray(sidecars.sidecars, "sidecar_list.sidecars");

    await invokeExpectError(
      child,
      "session/prompt",
      { request: {} },
      "invalid_request",
    );
    const agentInput = await invoke(child, "session/prompt", {
      request: {
        sessionId: workspaceSession.id,
        message: "Say hello",
      },
    });
    assertRecord(agentInput, "session/prompt result");
    if (
      typeof agentInput.agentRunId !== "string" ||
      !agentInput.agentRunId.trim() ||
      agentInput.sessionId !== workspaceSession.id
    ) {
      fail("session/prompt did not return agentRunId/sessionId");
    }
    const agentInputError = await waitForEvent(
      (event) =>
        event.eventName === "session/update" &&
        event.payload?.agentRunId === agentInput.agentRunId &&
        event.payload?.payload?.type === "runtime_event" &&
        event.payload?.payload?.event?.type === "RuntimeError",
      "session/prompt missing global model stream error",
    );
    if (
      !String(
        agentInputError.payload?.payload?.event?.payload?.message ?? "",
      ).includes("global model is not configured")
    ) {
      fail("session/prompt missing global model stream error was not explicit");
    }

    const mockModelServer = await startOpenAiCompatibleMockServer();
    try {
      const configuredSession = await invoke(child, "session/new", {
        request: {
          title: "configured workspace smoke session",
          cwd: workspaceRoot,
        },
      });
      assertRecord(configuredSession, "configured session/new result");
      await invoke(child, "agent_runtime_config_set", {
        request: {
          customModelProviders: [{
            providerId: "custom.smoke",
            name: "Smoke OpenAI-Compatible",
            baseUrl: mockModelServer.baseUrl,
            api: "openai-completions",
            models: [{
              model: "smoke-model",
              displayName: "Smoke Model",
              contextTokens: "32k",
              maxOutputTokens: "4k",
              supportsVision: false,
            }],
          }],
        },
      });
      const configEventCountBeforeFailure = events.filter(
        (event) => event.eventName === "runtime/config-changed",
      ).length;
      await invokeExpectError(child, "agent_runtime_config_set", {
        request: {
          modelProviderId: "custom.smoke",
          modelApiKey: " ",
        },
      }, "runtime_config_failed");
      if (
        events.filter((event) => event.eventName === "runtime/config-changed").length
        !== configEventCountBeforeFailure
      ) {
        fail("failed agent_runtime_config_set emitted runtime/config-changed");
      }
      await invoke(child, "agent_runtime_config_set", {
        request: {
          modelProviderId: "custom.smoke",
          modelApiKey: "smoke-secret",
        },
      });
      const configuredModel = await invoke(child, "agent_runtime_config_set", {
        request: {
          modelProviderId: "custom.smoke",
          model: "smoke-model",
        },
      });
      assertRecord(configuredModel, "configured session/prompt model config");
      const smokeProvider = configuredModel.modelProviders?.find(
        (provider) => provider.providerId === "custom.smoke",
      );
      if (smokeProvider?.configured !== true || smokeProvider.credentialSource !== "stored") {
        fail("configured model Provider credential was not acknowledged");
      }
      if (
        configuredModel.selectableModels?.length !== 1 ||
        configuredModel.selectableModels[0]?.model !== "smoke-model"
      ) {
        fail("configured model was not returned by the global model collection");
      }
      const configuredInput = await invoke(child, "session/prompt", {
        request: {
          sessionId: configuredSession.id,
          message: "Say hello through configured key",
        },
      });
      assertRecord(configuredInput, "configured session/prompt result");
      if (
        typeof configuredInput.agentRunId !== "string" ||
        !configuredInput.agentRunId.trim()
      ) {
        fail("configured session/prompt did not return agentRunId");
      }
      await waitForAgentRunDoneOrError(
        configuredInput.agentRunId,
        "configured session/prompt",
      );
      const modelRequest = mockModelServer.requests[0];
      if (!modelRequest) {
        fail("configured session/prompt did not call mock model server");
      }
      if (modelRequest.authorization !== "Bearer smoke-secret") {
        fail("configured session/prompt did not send persisted API key auth header");
      }
      const modelTest = await invoke(child, "agent_runtime_model_test", {
        request: { providerId: "custom.smoke", model: "smoke-model" },
      });
      assertRecord(modelTest, "agent_runtime_model_test result");
      if (modelTest.httpStatus !== 200 || modelTest.outputPreview !== "OK") {
        fail("agent_runtime_model_test did not return the mock model response");
      }

      const recoverySession = await invoke(child, "session/new", {
        request: {
          title: "runtime crash recovery smoke session",
          cwd: workspaceRoot,
        },
      });
      assertRecord(recoverySession, "runtime crash recovery session/new result");
      const recoveryInput = await invoke(child, "session/prompt", {
        request: {
          sessionId: recoverySession.id,
          message: "runtime crash recovery smoke",
        },
      });
      await waitForEvent(
        (event) =>
          event.eventName === "session/update" &&
          event.payload?.agentRunId === recoveryInput.agentRunId &&
          event.payload?.payload?.type === "runtime_event" &&
          event.payload?.payload?.event?.type === "ModelTextDelta",
        "session/prompt live text before runtime crash",
      );
      await fs.rm(workspaceRoot, { recursive: true, force: true });
      await restartRuntimeServer();
      const recoveryProjection = await invoke(child, "_centaeris/session/project", {
        request: { sessionId: recoverySession.id },
      });
      assertRecord(recoveryProjection, "runtime crash recovery session projection");
      const recoveredAssistant = recoveryProjection.session?.messages?.find(
        (message) => message.role === "assistant" && message.agentRunId === recoveryInput.agentRunId,
      );
      if (
        recoveredAssistant?.content !== "Hello from smoke" ||
        recoveredAssistant?.status !== "error"
      ) {
        fail(
          `runtime restart did not seal the live assistant text as error: ${JSON.stringify(recoveredAssistant)}`,
        );
      }
      const recoveredAgentRun = recoveryProjection.agentRuns?.find(
        (agentRun) => agentRun.agentRunId === recoveryInput.agentRunId,
      );
      if (
        recoveredAgentRun?.status !== "cancelled" ||
        recoveryProjection.activeAgentRunId !== null
      ) {
        fail("runtime restart did not terminally cancel the interrupted AgentRun");
      }
      const liveTextFiles = await fs.readdir(path.join(tempRoot, "runtime", "live-text"));
      if (liveTextFiles.length !== 0) {
        fail("runtime restart did not seal the recovered live text journal");
      }

    } finally {
      await mockModelServer.close();
    }

    for (const command of [
      "_centaeris/session/supplement",
      "_centaeris/session/answer_now",
      "_centaeris/session/answer_question",
    ]) {
      await invokeExpectError(child, command, { request: {} }, "invalid_request");
    }
    await writeRawFrameExpectError(
      child,
      "banana-envelope-1",
      {
        id: "banana-envelope-1",
        command: "workspace_get",
        payload: {},
      },
      -32600,
    );

    const configEventCountBeforeReset = events.filter(
      (event) => event.eventName === "runtime/config-changed",
    ).length;
    const resetConfig = await invoke(child, "agent_runtime_config_reset", {
      request: { confirm: true },
    });
    assertRecord(resetConfig?.config, "agent_runtime_config_reset result");
    if (
      events.filter((event) => event.eventName === "runtime/config-changed").length
      !== configEventCountBeforeReset + 1
    ) {
      fail("agent_runtime_config_reset did not emit one runtime/config-changed");
    }

    const exitResult = await invoke(child, "app_exit", {});
    assertRecord(exitResult, "app_exit result");
    if (exitResult.ok !== true) {
      fail("app_exit did not acknowledge ok=true");
    }

    socket.end();
    const runtimeExit = await waitForExit(server);
    if (runtimeExit.code !== 0) {
      fail(`idle Runtime Host exited with code ${runtimeExit.code}`);
    }
    server = null;
    await assertUnsupportedRuntimeStoreFailsBeforeListening(tempRoot, runtimeEnvironment);
    await assertHostTransportIsolatesInvalidLiveText(tempRoot);
    await assertHostTransportReconnectsAfterRuntimeExit(tempRoot);
    await assertHostTransportReplacesMismatchedRuntime(tempRoot);
    console.log(
      JSON.stringify(
        {
          ok: true,
          runtimeExe,
          tempRoot,
          checkedCommands: [
            "initialize",
            "initialize during slow process_capture",
            "session/list",
            "_centaeris/session/diagnostics",
            "session/new missing cwd failure",
            "workspace_activate",
            "workspace_get",
            "workspace_file_tree",
            "workspace_read_file",
            "desktop_file_preview_read absolute text",
            "desktop_file_preview_read relative text",
            "desktop_file_preview_read PDF",
            "workspace session/new",
            "session delete",
            "workspace session/list",
            "workspace session/load",
            "workspace session/activate",
            "workspace session/project",
            "workspace_file_tree with sessionId",
            "workspace_open_folder failure",
            "agent_runtime_config_get",
            "agent_runtime_config_set",
            "mcp/catalog",
            "runtime/config-changed",
            "agent_runtime_config_set platform Bash",
            "agent_runtime_config_set empty model key failure",
            "failed config mutation emits no notification",
            "agent_context_usage_get",
            "agent_state_get",
            "_centaeris/session/agent-runs",
            "agent_runtime_job_list",
            "agent_dead_letter_list",
            "agent_runtime_garbage_collect",
            "transcript/project",
            "sidecar_list",
            "session/prompt missing global model stream failure",
            "session/prompt configured key stream success",
            "agent_runtime_model_test HTTP success",
            "runtime crash live text recovery",
            "Runtime Host invalid live text isolation",
            "Runtime Host kill, reconnect, initialize replay, and session continuation",
            "Runtime Host build mismatch orderly replacement",
            "_centaeris/session/supplement invalid request",
            "_centaeris/session/answer_now invalid request",
            "_centaeris/session/answer_question invalid request",
            "non-JSON-RPC command envelope loud-fail",
            "agent_runtime_config_reset",
            "app_exit",
            "idle Runtime Host exit",
            "unsupported Runtime store fails before listening without modifying the database",
          ],
          observedEvents: events.length,
        },
        null,
        2,
      ),
    );
  } finally {
    lines?.close();
    socket?.destroy();
    if (server && server.exitCode === null && server.signalCode === null) {
      server.kill();
      await waitForExit(server);
    }
    await removeTempRoot(tempRoot);
  }
};

await main();
