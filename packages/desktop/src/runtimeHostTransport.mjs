import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import net from "node:net";
import { requireHostEventName } from "./hostContract.mjs";

const RUNTIME_SERVER_CONNECT_ATTEMPTS = 50;
const RUNTIME_SERVER_CONNECT_DELAY_MS = 100;
const RUNTIME_SERVER_ENDPOINT_TIMEOUT_MS = 5_000;
const RUNTIME_STALE_SERVER_SHUTDOWN_MS = 7_000;
const RUNTIME_APP_EXIT_TIMEOUT_MS = 2_000;
const EXPECTED_CORE_PROTOCOL_VERSION = "1.0.0";
const RUNTIME_DESCRIPTOR_FIELDS = new Set([
  "status",
  "runtime",
  "protocol",
  "protocolVersion",
  "capabilities",
  "events",
  "projections",
  "buildId",
  "coreProtocolVersion",
  "profileId",
  "storeId",
  "storeSchemaVersion",
  "layoutSchemaVersion",
]);

const hasOwn = (value, key) => Object.prototype.hasOwnProperty.call(value, key);

const sleep = (delayMs) => new Promise((resolve) => setTimeout(resolve, delayMs));

const runtimeExecutableBuildId = (executablePath) =>
  new Promise((resolve, reject) => {
    const hash = createHash("sha256");
    const input = createReadStream(executablePath);
    input.on("error", reject);
    input.on("data", (chunk) => hash.update(chunk));
    input.on("end", () => resolve(`sha256:${hash.digest("hex")}`));
  });

const validateRuntimeDescriptor = (descriptor, expectedBuildId) => {
  if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor)) {
    throw new Error("Runtime initialize response must be an object");
  }
  const unknownFields = Object.keys(descriptor).filter(
    (field) => !RUNTIME_DESCRIPTOR_FIELDS.has(field),
  );
  if (unknownFields.length > 0) {
    throw new Error(`Runtime initialize response has unknown fields: ${unknownFields.join(", ")}`);
  }
  const exact = {
    status: "ok",
    runtime: "centaeris-runtime",
    protocol: "centaeris.runtime",
    protocolVersion: 1,
    coreProtocolVersion: EXPECTED_CORE_PROTOCOL_VERSION,
  };
  for (const [field, expected] of Object.entries(exact)) {
    if (descriptor[field] !== expected) {
      throw new Error(`Runtime initialize ${field} mismatch: expected ${expected}, got ${descriptor[field]}`);
    }
  }
  for (const field of ["profileId", "storeId"]) {
    if (typeof descriptor[field] !== "string" || !descriptor[field].trim()) {
      throw new Error(`Runtime initialize ${field} is required`);
    }
  }
  for (const field of ["storeSchemaVersion", "layoutSchemaVersion"]) {
    if (!Number.isInteger(descriptor[field]) || descriptor[field] < 1) {
      throw new Error(`Runtime initialize ${field} must be a positive integer`);
    }
  }
  const requiredArrayItems = {
    capabilities: ["json_rpc_2_over_jsonl"],
    events: ["session/update", "runtime/config-changed"],
    projections: ["runtime_event", "session_event", "headless_transcript"],
  };
  for (const [field, requiredItems] of Object.entries(requiredArrayItems)) {
    const values = descriptor[field];
    if (!Array.isArray(values) || values.some((value) => typeof value !== "string" || !value)) {
      throw new Error(`Runtime initialize ${field} must be an array of non-empty strings`);
    }
    for (const item of requiredItems) {
      if (!values.includes(item)) {
        throw new Error(`Runtime initialize ${field} is missing ${item}`);
      }
    }
  }
  if (descriptor.buildId !== expectedBuildId) {
    throw new Error(`Runtime build mismatch: expected ${expectedBuildId}, got ${descriptor.buildId}`);
  }
  return descriptor;
};

export const requireRuntimeDescriptor = (descriptor, expectedBuildId) => {
  try {
    return validateRuntimeDescriptor(descriptor, expectedBuildId);
  } catch (error) {
    error.code = "runtime_descriptor_mismatch";
    throw error;
  }
};

export const stopStaleRuntimeSocket = async ({
  socket,
  requestAppExit,
  appExitTimeoutMs = RUNTIME_APP_EXIT_TIMEOUT_MS,
  staleServerShutdownMs = RUNTIME_STALE_SERVER_SHUTDOWN_MS,
}) => {
  let gracefulExit = false;
  let exitTimeout;
  try {
    await Promise.race([
      requestAppExit().then(() => {
        gracefulExit = true;
      }),
      new Promise((resolve) => {
        exitTimeout = setTimeout(resolve, appExitTimeoutMs);
      }),
    ]);
  } catch {
    // Replacement is best effort: a stale Runtime may reject or never answer app_exit.
  } finally {
    clearTimeout(exitTimeout);
  }

  if (gracefulExit) socket.end();
  else socket.destroy();
  await new Promise((resolve) => {
    if (socket.destroyed) {
      resolve();
      return;
    }
    const timeout = setTimeout(() => {
      socket.destroy();
      resolve();
    }, appExitTimeoutMs);
    socket.once("close", () => {
      clearTimeout(timeout);
      resolve();
    });
  });
  await sleep(staleServerShutdownMs);
};

export const createRuntimeHostTransport = ({
  executablePath,
  cwd,
  emitHostEvent,
  showFailureDialog,
  isAppReady,
  isQuitting,
  isSmokeRun = false,
  environment = process.env,
  onRuntimeServerStarted,
}) => {
  if (!executablePath) {
    throw new Error("Runtime Host executable path is required");
  }
  if (!cwd) {
    throw new Error("Runtime Host cwd is required");
  }
  if (typeof emitHostEvent !== "function") {
    throw new Error("Runtime Host transport requires emitHostEvent");
  }

  let isShuttingDown = false;
  let runtimeSocket = null;
  let runtimeSocketBuffer = "";
  let runtimeSocketWriteQueue = Promise.resolve();
  let runtimeConnectPromise = null;
  let runtimeServerStartFailure = null;
  let runtimeInitialization = null;
  let runtimeInitializationResult = null;
  let runtimeInitializedSocket = null;
  let runtimeReadyPromise = null;
  let runtimeRestarting = false;
  let expectedBuildIdPromise = null;
  let nextRuntimeRequestId = 1;
  const pendingHostRequests = new Map();

  const rejectPendingHostRequests = (error) => {
    for (const pending of pendingHostRequests.values()) {
      pending.reject(error);
    }
    pendingHostRequests.clear();
  };

  const emitRuntimeFailure = (message) => {
    const payload = { message, tsMs: Date.now() };
    emitHostEvent("centaeris/runtime-host-error", payload);
    if (!isSmokeRun && !isQuitting?.() && isAppReady?.()) {
      showFailureDialog?.(message, "");
    }
  };

  const writeSocket = async (socket, frame) => {
    if (!socket.writable) {
      throw new Error("Runtime Server socket is not writable");
    }
    const serialized = `${JSON.stringify(frame)}\n`;
    await new Promise((resolve, reject) => {
      socket.write(serialized, (error) => (error ? reject(error) : resolve()));
    });
  };

  const enqueueSocketWrite = async (frame, expectedSocket = null) => {
    const socket = expectedSocket ?? await ensureRuntimeSocket();
    if (runtimeSocket !== socket || socket.destroyed) {
      throw new Error("Runtime Server connection closed before command write");
    }
    const writeTask = runtimeSocketWriteQueue.then(() => writeSocket(socket, frame));
    runtimeSocketWriteQueue = writeTask.catch(() => {});
    await writeTask;
  };

  const writeRuntimeMethodNotFound = async (id, method) => {
    await enqueueSocketWrite({
      jsonrpc: "2.0",
      id,
      error: {
        code: -32601,
        message: `runtime request method not supported: ${method}`,
      },
    });
  };

  const handleRuntimeRequest = async (message) => {
    if (hasOwn(message, "result") || hasOwn(message, "error")) {
      throw new Error("Runtime Server request must not include result or error");
    }
    const requestId = String(message.id ?? "").trim();
    if (!requestId) {
      throw new Error("Runtime Server request id is required");
    }
    const method = message.method.trim();
    await writeRuntimeMethodNotFound(message.id, method);
  };

  const handleRuntimeLine = async (line) => {
    if (!line.trim()) {
      return;
    }
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      throw new Error(`invalid Runtime Server JSONL message: ${error.message}`);
    }
    if (message?.jsonrpc !== "2.0") {
      throw new Error("Runtime Server message missing JSON-RPC 2.0 envelope");
    }
    if (typeof message.method === "string" && hasOwn(message, "id")) {
      await handleRuntimeRequest(message);
      return;
    }
    if (typeof message.method === "string") {
      requireHostEventName(message.method);
      emitHostEvent(message.method, message.params ?? {});
      return;
    }
    const hasResult = hasOwn(message, "result");
    const hasError = hasOwn(message, "error");
    if (!hasOwn(message, "id") || hasResult === hasError) {
      throw new Error("Runtime Server message must be a JSON-RPC response or notification");
    }
    const responseId = String(message.id);
    const pending = pendingHostRequests.get(responseId);
    if (!pending) {
      throw new Error(`Runtime Server response has no pending request: ${responseId}`);
    }
    pendingHostRequests.delete(responseId);
    if (hasResult) {
      pending.resolve(message.result);
      return;
    }
    const errorMessage =
      typeof message.error?.message === "string"
        ? message.error.message
        : typeof message.error === "string"
          ? message.error
          : "Runtime Server command failed";
    const error = new Error(errorMessage);
    error.code =
      typeof message.error?.data?.code === "string"
        ? message.error.data.code
        : message.error?.code;
    error.payload = message.error;
    pending.reject(error);
  };

  const attachSocket = (socket) => {
    runtimeSocket = socket;
    runtimeSocketBuffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      runtimeSocketBuffer += chunk;
      const lines = runtimeSocketBuffer.split("\n");
      runtimeSocketBuffer = lines.pop() ?? "";
      for (const line of lines) {
        void handleRuntimeLine(line).catch((error) => {
          rejectPendingHostRequests(error);
          emitRuntimeFailure(`Runtime Server protocol violation: ${error.message}`);
          socket.destroy();
        });
      }
    });
    socket.on("error", (error) => {
      if (runtimeSocket === socket && !isShuttingDown && !runtimeRestarting) {
        emitRuntimeFailure(`Runtime Server connection failed: ${error.message}`);
      }
    });
    socket.on("close", () => {
      if (runtimeSocket !== socket) {
        return;
      }
      runtimeSocket = null;
      runtimeInitializedSocket = null;
      runtimeSocketBuffer = "";
      runtimeSocketWriteQueue = Promise.resolve();
      rejectPendingHostRequests(new Error("Runtime Server connection closed"));
      if (!isShuttingDown && !runtimeRestarting && !isQuitting?.()) {
        emitRuntimeFailure("Runtime Server connection closed");
      }
    });
    return socket;
  };

  const connectSocket = (endpoint) =>
    new Promise((resolve, reject) => {
      const socket = net.createConnection(endpoint);
      const rejectOnce = (error) => {
        socket.off("connect", connectOnce);
        reject(error);
      };
      const connectOnce = () => {
        socket.off("error", rejectOnce);
        resolve(attachSocket(socket));
      };
      socket.once("error", rejectOnce);
      socket.once("connect", connectOnce);
    });

  const runtimeServerEndpoint = () =>
    new Promise((resolve, reject) => {
      const probe = spawn(executablePath, ["--runtime-server-endpoint"], {
        cwd,
        env: environment,
        stdio: ["ignore", "pipe", "pipe"],
        windowsHide: true,
      });
      let stdout = "";
      let stderr = "";
      const timeout = setTimeout(() => {
        probe.kill();
        reject(new Error("Runtime Server endpoint discovery timed out"));
      }, RUNTIME_SERVER_ENDPOINT_TIMEOUT_MS);
      probe.stdout.on("data", (chunk) => {
        stdout += chunk;
      });
      probe.stderr.on("data", (chunk) => {
        stderr += chunk;
      });
      probe.once("error", (error) => {
        clearTimeout(timeout);
        reject(error);
      });
      probe.once("exit", (code) => {
        clearTimeout(timeout);
        if (code !== 0) {
          reject(new Error(`Runtime Server endpoint discovery failed: ${stderr.trim()}`));
          return;
        }
        try {
          const endpoint = String(JSON.parse(stdout).endpoint ?? "").trim();
          if (!endpoint) {
            throw new Error("Runtime Server endpoint descriptor is missing endpoint");
          }
          resolve(endpoint);
        } catch (error) {
          reject(new Error(`invalid Runtime Server endpoint descriptor: ${error.message}`));
        }
      });
    });

  const startRuntimeServer = () => {
    const serverProcess = spawn(executablePath, ["--runtime-server"], {
      cwd,
      env: environment,
      detached: true,
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
    });
    onRuntimeServerStarted?.(serverProcess);
    let stderrTail = "";
    serverProcess.stderr.setEncoding("utf8");
    serverProcess.stderr.on("data", (chunk) => {
      stderrTail = `${stderrTail}${chunk}`.slice(-8_192);
    });
    serverProcess.once("error", (error) => {
      runtimeServerStartFailure = new Error(`Runtime Server start failed: ${error.message}`);
    });
    serverProcess.once("close", (code, signal) => {
      const detail = stderrTail.trim();
      if (detail || !runtimeServerStartFailure) {
        runtimeServerStartFailure = new Error(
          detail ||
            `Runtime Server exited before accepting a connection (${signal ? `signal ${signal}` : `code ${code}`})`,
        );
      }
    });
    serverProcess.stderr.unref();
    serverProcess.unref();
  };

  const ensureRuntimeSocket = async () => {
    if (runtimeSocket && !runtimeSocket.destroyed) {
      return runtimeSocket;
    }
    if (runtimeConnectPromise) {
      return runtimeConnectPromise;
    }
    runtimeConnectPromise = (async () => {
      const endpoint = await runtimeServerEndpoint();
      try {
        return await connectSocket(endpoint);
      } catch {
        runtimeServerStartFailure = null;
        startRuntimeServer();
      }
      let lastError = new Error("Runtime Server did not accept a connection");
      for (let attempt = 0; attempt < RUNTIME_SERVER_CONNECT_ATTEMPTS; attempt += 1) {
        if (runtimeServerStartFailure) {
          throw runtimeServerStartFailure;
        }
        try {
          return await connectSocket(endpoint);
        } catch (error) {
          lastError = error;
          await sleep(RUNTIME_SERVER_CONNECT_DELAY_MS);
        }
      }
      throw runtimeServerStartFailure ?? lastError;
    })();
    try {
      return await runtimeConnectPromise;
    } finally {
      runtimeConnectPromise = null;
    }
  };

  const invokeCommandOnSocket = async (command, payload, metadata, socket) => {
    const id = `electron-${nextRuntimeRequestId++}`;
    const request = {
      jsonrpc: "2.0",
      id,
      method: command,
      params: payload ?? {},
    };
    return new Promise((resolve, reject) => {
      pendingHostRequests.set(id, { resolve, reject, command, group: metadata.group });
      void enqueueSocketWrite(request, socket).catch((error) => {
        pendingHostRequests.delete(id);
        reject(error);
      });
    });
  };

  const expectedBuildId = () => {
    expectedBuildIdPromise ??= runtimeExecutableBuildId(executablePath);
    return expectedBuildIdPromise;
  };

  const stopStaleRuntime = async (socket) => {
    runtimeRestarting = true;
    await stopStaleRuntimeSocket({
      socket,
      requestAppExit: () => invokeCommandOnSocket("app_exit", {}, { group: "app" }, socket),
    });
  };

  const ensureRuntimeReady = async () => {
    const socket = await ensureRuntimeSocket();
    if (!runtimeInitialization || runtimeInitializedSocket === socket) {
      return socket;
    }
    if (!runtimeReadyPromise) {
      runtimeReadyPromise = (async () => {
        const requiredBuildId = await expectedBuildId();
        for (let attempt = 0; attempt < 2; attempt += 1) {
          const readySocket = await ensureRuntimeSocket();
          if (runtimeInitializedSocket === readySocket) {
            return readySocket;
          }
          runtimeInitializationResult = await invokeCommandOnSocket(
            "initialize",
            runtimeInitialization.payload,
            runtimeInitialization.metadata,
            readySocket,
          );
          try {
            requireRuntimeDescriptor(runtimeInitializationResult, requiredBuildId);
          } catch (error) {
            if (error.code !== "runtime_descriptor_mismatch" || attempt > 0) {
              throw error;
            }
            await stopStaleRuntime(readySocket);
            continue;
          }
          if (runtimeSocket !== readySocket || readySocket.destroyed) {
            throw new Error("Runtime Server connection closed during initialize");
          }
          runtimeRestarting = false;
          runtimeInitializedSocket = readySocket;
          return readySocket;
        }
        throw new Error("Runtime Server did not become ready");
      })();
    }
    try {
      return await runtimeReadyPromise;
    } finally {
      runtimeReadyPromise = null;
      runtimeRestarting = false;
    }
  };

  const invokeCommand = async (command, payload, metadata = {}) => {
    if (isShuttingDown && command !== "app_exit") {
      throw new Error("Runtime Server host connection is shutting down, command rejected");
    }
    if (command === "initialize") {
      runtimeInitialization = { payload: payload ?? {}, metadata };
      runtimeInitializedSocket = null;
      await ensureRuntimeReady();
      return runtimeInitializationResult;
    }
    const socket = await ensureRuntimeReady();
    return invokeCommandOnSocket(command, payload, metadata, socket);
  };

  const requestAppExit = async () => {
    if (isShuttingDown) {
      return;
    }
    isShuttingDown = true;
    if (!runtimeSocket || runtimeSocket.destroyed) {
      return;
    }
    let timeoutId = null;
    try {
      await Promise.race([
        invokeCommand("app_exit", {}, { group: "app" }),
        new Promise((_, reject) => {
          timeoutId = setTimeout(
            () => reject(new Error("Runtime Server app_exit timed out")),
            RUNTIME_APP_EXIT_TIMEOUT_MS,
          );
        }),
      ]);
    } catch {
      // A host crash or socket close still produces the same owner-disconnect signal server-side.
    } finally {
      if (timeoutId !== null) {
        clearTimeout(timeoutId);
      }
      runtimeSocket.end();
    }
  };

  return {
    invokeCommand,
    requestAppExit,
    isShuttingDown: () => isShuttingDown,
  };
};
