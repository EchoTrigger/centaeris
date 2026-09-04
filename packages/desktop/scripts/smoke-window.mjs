import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const hostRoot = path.resolve(import.meta.dirname, "..");
const defaultElectronExe = path.join(
  hostRoot,
  "dist",
  "Centaeris Desktop",
  "Centaeris Desktop.exe",
);
const electronExe =
  process.env.CENTAERIS_ELECTRON_SMOKE_EXE || defaultElectronExe;
const runtimeExe = path.join(
  path.dirname(electronExe),
  "resources",
  "bin",
  process.platform === "win32" ? "centaeris-runtime.exe" : "centaeris-runtime",
);
const CONNECT_TIMEOUT_MS = 30_000;
const EVALUATE_TIMEOUT_MS = 20_000;
const PROCESS_TIMEOUT_MS = 30_000;
const RUNTIME_SERVER_IDLE_CLEANUP_MS = 7_000;

let stderrTail = "";

const fail = (message) => {
  throw new Error(`${message}${stderrTail ? `\nElectron stderr:\n${stderrTail}` : ""}`);
};

const appendStderr = (chunk) => {
  stderrTail = `${stderrTail}${chunk.toString()}`.slice(-12_000);
};

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const resolveRuntimeServerEndpoint = (runtimeDataDir) =>
  new Promise((resolve, reject) => {
    const child = spawn(runtimeExe, ["--runtime-server-endpoint"], {
      cwd: path.dirname(runtimeExe),
      env: { ...process.env, CENTAERIS_DESKTOP_DATA_DIR: runtimeDataDir },
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.once("error", reject);
    child.once("exit", (code) => {
      if (code !== 0) {
        reject(new Error(`Runtime endpoint discovery failed: ${stderr.trim()}`));
        return;
      }
      try {
        const endpoint = String(JSON.parse(stdout).endpoint ?? "").trim();
        if (!endpoint) {
          throw new Error("Runtime endpoint descriptor is missing endpoint");
        }
        resolve(endpoint);
      } catch (error) {
        reject(error);
      }
    });
  });

const assertRuntimeServerExited = async (runtimeDataDir) => {
  const endpoint = await resolveRuntimeServerEndpoint(runtimeDataDir);
  const connected = await new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint);
    const timeout = setTimeout(() => {
      socket.destroy();
      reject(new Error("Runtime endpoint exit probe timed out"));
    }, 2_000);
    socket.once("connect", () => {
      clearTimeout(timeout);
      socket.destroy();
      resolve(true);
    });
    socket.once("error", (error) => {
      clearTimeout(timeout);
      if (error.code === "ENOENT" || error.code === "ECONNREFUSED") {
        resolve(false);
        return;
      }
      reject(error);
    });
  });
  if (connected) {
    fail("Runtime Server still accepts connections after the idle cleanup window");
  }
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

const findFreePort = async () =>
  new Promise((resolve, reject) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        if (!address || typeof address === "string") {
          reject(new Error("could not allocate a localhost port"));
          return;
        }
        resolve(address.port);
      });
    });
    server.on("error", reject);
  });

const readJson = (url) =>
  new Promise((resolve, reject) => {
    const request = http.get(url, (response) => {
      let body = "";
      response.setEncoding("utf8");
      response.on("data", (chunk) => {
        body += chunk;
      });
      response.on("end", () => {
        if ((response.statusCode ?? 500) >= 400) {
          reject(new Error(`${url} returned HTTP ${response.statusCode}`));
          return;
        }
        try {
          resolve(JSON.parse(body));
        } catch (error) {
          reject(error);
        }
      });
    });
    request.on("error", reject);
    request.setTimeout(2_000, () => {
      request.destroy(new Error(`${url} timed out`));
    });
  });

const waitForPageTarget = async (port) => {
  const startedAt = Date.now();
  let lastError = null;
  while (Date.now() - startedAt < CONNECT_TIMEOUT_MS) {
    try {
      const targets = await readJson(`http://127.0.0.1:${port}/json/list`);
      const page = targets.find(
        (target) =>
          target.type === "page" &&
          typeof target.webSocketDebuggerUrl === "string",
      );
      if (page) {
        return page;
      }
    } catch (error) {
      lastError = error;
    }
    await delay(250);
  }
  fail(
    `Electron page target was not available on DevTools port ${port}: ${
      lastError?.message ?? "unknown error"
    }`,
  );
};

class CdpClient {
  constructor(webSocketUrl) {
    this.nextId = 1;
    this.pending = new Map();
    this.socket = new WebSocket(webSocketUrl);
  }

  async open() {
    if (this.socket.readyState === WebSocket.OPEN) {
      return;
    }
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        cleanup();
        reject(new Error("CDP websocket open timed out"));
      }, CONNECT_TIMEOUT_MS);
      const cleanup = () => {
        clearTimeout(timeout);
        this.socket.removeEventListener("open", onOpen);
        this.socket.removeEventListener("error", onError);
        this.socket.removeEventListener("message", onMessage);
        this.socket.removeEventListener("close", onClose);
      };
      const onOpen = () => {
        cleanup();
        this.socket.addEventListener("message", (event) =>
          this.handleMessage(event),
        );
        resolve();
      };
      const onError = () => {
        cleanup();
        reject(new Error("CDP websocket failed to open"));
      };
      const onMessage = (event) => this.handleMessage(event);
      const onClose = () => {
        cleanup();
        reject(new Error("CDP websocket closed before open"));
      };
      this.socket.addEventListener("open", onOpen);
      this.socket.addEventListener("error", onError);
      this.socket.addEventListener("message", onMessage);
      this.socket.addEventListener("close", onClose);
    });
  }

  handleMessage(event) {
    let message;
    try {
      message = JSON.parse(event.data);
    } catch {
      return;
    }
    if (!message.id) {
      return;
    }
    const pending = this.pending.get(message.id);
    if (!pending) {
      return;
    }
    this.pending.delete(message.id);
    if (message.error) {
      pending.reject(new Error(message.error.message || "CDP command failed"));
      return;
    }
    pending.resolve(message.result);
  }

  call(method, params = {}, timeoutMs = EVALUATE_TIMEOUT_MS) {
    const id = this.nextId++;
    const payload = JSON.stringify({ id, method, params });
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timeout);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timeout);
          reject(error);
        },
      });
      this.socket.send(payload);
    });
  }

  close() {
    this.socket.close();
  }
}

const evaluate = async (client, expression, timeoutMs = EVALUATE_TIMEOUT_MS) => {
  const result = await client.call(
    "Runtime.evaluate",
    {
      expression,
      awaitPromise: true,
      returnByValue: true,
      timeout: timeoutMs,
    },
    timeoutMs + 2_000,
  );
  if (result.exceptionDetails) {
    fail(
      `renderer evaluation failed: ${
        result.exceptionDetails.text || "unknown exception"
      }`,
    );
  }
  return result.result?.value;
};

const waitForRendererReady = async (client) => {
  const startedAt = Date.now();
  while (Date.now() - startedAt < CONNECT_TIMEOUT_MS) {
    const ready = await evaluate(
      client,
      `Boolean(document.readyState === "complete" && window.centaerisHost && window.centaerisHost.kind === "electron")`,
      5_000,
    );
    if (ready) {
      return;
    }
    await delay(250);
  }
  fail("renderer did not expose window.centaerisHost in time");
};

const waitForExit = async (child) => {
  if (child.exitCode !== null || child.signalCode !== null) {
    return { code: child.exitCode, signal: child.signalCode };
  }
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`Electron did not exit within ${PROCESS_TIMEOUT_MS}ms`));
    }, PROCESS_TIMEOUT_MS);
    child.once("exit", (code, signal) => {
      clearTimeout(timeout);
      resolve({ code, signal });
    });
  });
};

const waitForSecondInstanceExit = async (child) => {
  if (child.exitCode !== null || child.signalCode !== null) {
    return { code: child.exitCode, signal: child.signalCode };
  }
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      child.kill();
      reject(
        new Error("second Centaeris Desktop.exe instance did not exit after activation"),
      );
    }, 8_000);
    child.once("exit", (code, signal) => {
      clearTimeout(timeout);
      resolve({ code, signal });
    });
  });
};

const verifySecondLaunchActivatesPrimaryInstance = async ({
  runtimeDataDir,
  userDataDir,
}) => {
  const child = spawn(
    electronExe,
    [
      `--user-data-dir=${userDataDir}`,
      "--no-first-run",
    ],
    {
      cwd: path.dirname(electronExe),
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
      env: {
        ...process.env,
        CENTAERIS_ELECTRON_SMOKE: "1",
        CENTAERIS_DESKTOP_DATA_DIR: runtimeDataDir,
        CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
        CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
        CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
        CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
        OPENAI_API_KEY: "",
        DEEPSEEK_API_KEY: "",
        KIMI_API_KEY: "",
        CENTAERIS_MODEL_API_KEY: "",
      },
    },
  );

  child.stderr.on("data", appendStderr);
  child.once("error", (error) => {
    fail(`failed to start second Electron instance: ${error.message}`);
  });

  const exit = await waitForSecondInstanceExit(child);
  if (exit.code !== 0) {
    fail(
      `second Centaeris Desktop.exe instance should exit after activating primary instance: code=${exit.code} signal=${
        exit.signal ?? "null"
      }`,
    );
  }
};

const runPackagedWindow = async ({
  runtimeDataDir,
  userDataDir,
  expression,
  checkedLabel,
  verifySingleInstance = false,
  exitMode = "renderer-command",
}) => {
  const devtoolsPort = await findFreePort();
  await fs.mkdir(userDataDir, { recursive: true });

  const child = spawn(
    electronExe,
    [
      `--remote-debugging-port=${devtoolsPort}`,
      `--user-data-dir=${userDataDir}`,
      "--no-first-run",
    ],
    {
      cwd: path.dirname(electronExe),
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
      env: {
        ...process.env,
        CENTAERIS_ELECTRON_SMOKE: "1",
        CENTAERIS_DESKTOP_DATA_DIR: runtimeDataDir,
        CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
        CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
        CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
        CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
        OPENAI_API_KEY: "",
        DEEPSEEK_API_KEY: "",
        KIMI_API_KEY: "",
        CENTAERIS_MODEL_API_KEY: "",
      },
    },
  );

  child.stderr.on("data", appendStderr);
  child.once("error", (error) => fail(`failed to start Electron: ${error.message}`));

  let client = null;
  try {
    const page = await waitForPageTarget(devtoolsPort);
    client = new CdpClient(page.webSocketDebuggerUrl);
    await client.open();
    await client.call("Runtime.enable");
    await waitForRendererReady(client);

    if (verifySingleInstance) {
      await verifySecondLaunchActivatesPrimaryInstance({
        runtimeDataDir,
        userDataDir,
      });
    }

    const smokeResult = await evaluate(client, expression, 30_000);

    assertRecord(smokeResult, "renderer smoke result");
    if (smokeResult.hostKind !== "electron") {
      fail("renderer host kind must be electron");
    }
    if (!smokeResult.bodyTextLength || smokeResult.bodyTextLength < 10) {
      fail("renderer body text is unexpectedly empty");
    }
    assertArray(smokeResult.sessions, "session/list result");
    assertRecord(smokeResult.config, "agent_runtime_config_get result");
    assertRecord(smokeResult.shell, "thin desktop shell");
    if (
      !smokeResult.shell.nativeTitlebar ||
      !smokeResult.shell.sidebar ||
      !smokeResult.shell.chatColumn ||
      smokeResult.shell.resourceButtons.join(",") !== "Models,Skills,Plugins" ||
      !smokeResult.shell.workspacePicker ||
      smokeResult.shell.hasOpenWorkspaceButton
    ) {
      fail(`renderer did not expose the native-titlebar thin workspace shell: ${JSON.stringify(smokeResult.shell)}`);
    }
    if (
      smokeResult.config.modelProviderId ||
      smokeResult.config.model ||
      smokeResult.config.selectableModels?.length
    ) {
      fail("packaged runtime config must not inject an implicit model");
    }
    assertRecord(smokeResult.tasks, "_centaeris/session/agent-runs result");
    assertArray(smokeResult.tasks.agentRuns, "_centaeris/session/agent-runs.agentRuns");
    assertRecord(smokeResult.projection, "transcript/project result");
    assertArray(smokeResult.projection.lines, "transcript/project.lines");
    const projectionKinds = smokeResult.projection.lines.map((line) => line.kind);
    if (!projectionKinds.includes("assistant_text") || !projectionKinds.includes("tool_group")) {
      fail("headless projection did not return assistant_text and tool_group lines");
    }
    assertRecord(smokeResult.sidecars, "sidecar_list result");
    assertArray(smokeResult.sidecars.sidecars, "sidecar_list.sidecars");
    assertRecord(smokeResult.contextUsage, "agent_context_usage_get result");
    assertRecord(smokeResult.agentState, "agent_state_get result");
    assertRecord(smokeResult.runtimeJobs, "agent_runtime_job_list result");
    assertArray(smokeResult.runtimeJobs.jobs, "agent_runtime_job_list.jobs");
    assertRecord(smokeResult.deadLetters, "agent_dead_letter_list result");
    assertArray(smokeResult.deadLetters.deadLetters, "agent_dead_letter_list.deadLetters");
    assertRecord(smokeResult.runtimeGarbage, "agent_runtime_garbage_collect result");
    if (smokeResult.runtimeGarbage.dryRun !== true) {
      fail("agent_runtime_garbage_collect did not run as dryRun");
    }
    assertArray(smokeResult.commandFailures, "command failure results");
    for (const failure of smokeResult.commandFailures) {
      assertRecord(failure, "command failure");
      if (failure.ok !== false || !String(failure.message).includes(failure.expected)) {
        fail(`${failure.command} did not fail loudly with ${failure.expected}`);
      }
    }
    assertArray(smokeResult.runtimeHostErrors, "centaeris/runtime-host-error captures");
    if (smokeResult.runtimeHostErrors.length > 0) {
      fail("renderer observed centaeris/runtime-host-error during smoke");
    }

    if (exitMode === "native") {
      await client.call("Browser.close").catch(() => undefined);
    } else {
      await evaluate(
        client,
        `window.centaerisHost.invoke("app_exit", {}).catch(() => undefined); true`,
        5_000,
      );
    }
    const exit = await waitForExit(child);
    client.close();
    client = null;
    if (exit.code !== 0) {
      fail(
        `${checkedLabel} Electron exited with code=${exit.code} signal=${
          exit.signal ?? "null"
        }`,
      );
    }

    return {
      ...smokeResult,
      projectionKinds,
    };
  } finally {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill();
      await waitForExit(child).catch(() => {});
    }
    client?.close();
  }
};

const FIRST_LAUNCH_EXPRESSION = `(async () => {
  const host = window.centaerisHost;
  const workspaceRoot = __WORKSPACE_ROOT__;
  const runtimeHostErrors = [];
  const runtimeConfigChanges = [];
  const expectCommandFailure = async (command, expected, payload = { request: {} }) => {
    try {
      await host.invoke(command, payload);
      return { command, expected, ok: true, message: "" };
    } catch (error) {
      return { command, expected, ok: false, message: String(error && error.message ? error.message : error) };
    }
  };
  const unsubscribe = await host.listen("centaeris/runtime-host-error", (payload) => {
    runtimeHostErrors.push(payload);
  });
  const unsubscribeConfig = await host.listen("runtime/config-changed", (payload) => {
    runtimeConfigChanges.push(payload);
  });
  const shell = {
    nativeTitlebar: Boolean(document.querySelector(".nativeTitlebar")),
    sidebar: Boolean(document.querySelector(".thinSidebar")),
    chatColumn: Boolean(document.querySelector(".thinChatColumn")),
    resourceButtons: Array.from(document.querySelectorAll(".thinSidebarFooter button span"))
      .map((element) => element.textContent?.trim())
      .filter(Boolean),
    workspacePicker: Boolean(document.querySelector(".thinWorkspaceSelectTrigger")),
    hasOpenWorkspaceButton: Boolean(document.querySelector(".thinOpenWorkspaceButton")),
  };
  const sessions = await host.invoke("session/list", {
    request: {},
  });
  const createdSession = await host.invoke("session/new", {
    request: {
      title: "Electron window smoke persisted session",
      cwd: workspaceRoot,
    },
  });
  const loadedSession = await host.invoke("session/load", {
    request: { sessionId: createdSession.id },
  });
  const config = await host.invoke("agent_runtime_config_get", {
    request: {},
  });
  const savedConfig = await host.invoke("agent_runtime_config_set", {
    request: { toolParallelism: 7 },
  });
  for (let attempt = 0; attempt < 100 && runtimeConfigChanges.length === 0; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  const tasks = await host.invoke("_centaeris/session/agent-runs", {});
  const projection = await host.invoke("transcript/project", {
    request: {
      streamItems: [
        {
          type: "session_event",
          event: {
            version: "v1",
            id: "smoke-tool-group-event",
            type: "Final",
            status: "done",
            payload: {
              content: "窗口 smoke 过程文本",
            },
          },
        },
        {
          type: "session_event",
          event: {
            version: "v1",
            id: "smoke-assistant-process",
            type: "ToolResult",
            status: "done",
            toolName: "bash",
            payload: {
              callId: "smoke-tool-group",
              summary: "运行命令",
              operations: [
                {
                  toolName: "bash",
                  kind: "command",
                  title: "Executed command",
                  commandPreview: "smoke command",
                  outputPreview: "ok",
                  exitCode: 0,
                },
              ],
            },
          },
        },
      ],
    },
  });
  const sidecars = await host.invoke("sidecar_list", {});
  const contextUsage = await host.invoke("agent_context_usage_get", {
    request: { sessionId: createdSession.id },
  });
  const agentState = await host.invoke("agent_state_get", {
    request: { sessionId: createdSession.id, includeRuntimeState: true },
  });
  const runtimeJobs = await host.invoke("agent_runtime_job_list", {
    request: { limit: 5 },
  });
  const deadLetters = await host.invoke("agent_dead_letter_list", {
    request: { limit: 5 },
  });
  const runtimeGarbage = await host.invoke("agent_runtime_garbage_collect", {
    request: { dryRun: true },
  });
  const commandFailures = [];
  for (const command of [
    "_centaeris/session/supplement",
    "_centaeris/session/answer_now",
    "_centaeris/session/answer_question",
  ]) {
    commandFailures.push(await expectCommandFailure(command, "missing field"));
  }
  commandFailures.push(
    await expectCommandFailure(
      "workspace_open_folder",
      "request.mode",
      { request: { mode: "banana" } },
    ),
  );
  unsubscribe();
  unsubscribeConfig();
  return {
    title: document.title,
    bodyTextLength: document.body.innerText.length,
    hostKind: host.kind,
    shell,
    sessions,
    config,
    savedConfig,
    createdSession,
    loadedSession,
    tasks,
    projection,
    sidecars,
    contextUsage,
    agentState,
    runtimeJobs,
    deadLetters,
    runtimeGarbage,
    commandFailures,
    runtimeHostErrors,
    runtimeConfigChanges,
  };
})()`;

const SECOND_LAUNCH_EXPRESSION = `(async () => {
  const host = window.centaerisHost;
  const workspaceRoot = __WORKSPACE_ROOT__;
  const sessionId = __SESSION_ID__;
  const runtimeHostErrors = [];
  const unsubscribe = await host.listen("centaeris/runtime-host-error", (payload) => {
    runtimeHostErrors.push(payload);
  });
  const sessions = await host.invoke("session/list", {
    request: {},
  });
  const shell = {
    nativeTitlebar: Boolean(document.querySelector(".nativeTitlebar")),
    sidebar: Boolean(document.querySelector(".thinSidebar")),
    chatColumn: Boolean(document.querySelector(".thinChatColumn")),
    resourceButtons: Array.from(document.querySelectorAll(".thinSidebarFooter button span"))
      .map((element) => element.textContent?.trim())
      .filter(Boolean),
    workspacePicker: Boolean(document.querySelector(".thinWorkspaceSelectTrigger")),
    hasOpenWorkspaceButton: Boolean(document.querySelector(".thinOpenWorkspaceButton")),
  };
  const config = await host.invoke("agent_runtime_config_get", {
    request: {},
  });
  const tasks = await host.invoke("_centaeris/session/agent-runs", {});
  const projection = await host.invoke("transcript/project", {
    request: {
      streamItems: [
        {
          type: "session_event",
          event: {
            version: "v1",
            id: "smoke-reopen-tool-event",
            type: "Final",
            status: "done",
            payload: {
              content: "窗口 smoke 重启恢复文本",
            },
          },
        },
        {
          type: "session_event",
          event: {
            version: "v1",
            id: "smoke-reopen-assistant",
            type: "ToolResult",
            status: "done",
            toolName: "bash",
            payload: {
              callId: "smoke-reopen-tool",
              summary: "运行命令",
              operations: [
                {
                  toolName: "bash",
                  kind: "command",
                  title: "Executed command",
                  commandPreview: "reopen smoke command",
                  outputPreview: "ok",
                  exitCode: 0,
                },
              ],
            },
          },
        },
      ],
    },
  });
  const sidecars = await host.invoke("sidecar_list", {});
  const contextUsage = await host.invoke("agent_context_usage_get", {
    request: { sessionId },
  });
  const agentState = await host.invoke("agent_state_get", {
    request: { sessionId, includeRuntimeState: true },
  });
  const runtimeJobs = await host.invoke("agent_runtime_job_list", {
    request: { limit: 5 },
  });
  const deadLetters = await host.invoke("agent_dead_letter_list", {
    request: { limit: 5 },
  });
  const runtimeGarbage = await host.invoke("agent_runtime_garbage_collect", {
    request: { dryRun: true },
  });
  const commandFailures = [];
  unsubscribe();
  return {
    title: document.title,
    bodyTextLength: document.body.innerText.length,
    hostKind: host.kind,
    shell,
    sessions,
    config,
    tasks,
    projection,
    sidecars,
    contextUsage,
    agentState,
    runtimeJobs,
    deadLetters,
    runtimeGarbage,
    commandFailures,
    runtimeHostErrors,
  };
})()`;

const main = async () => {
  await fs.access(electronExe);
  const tempRoot = await fs.mkdtemp(
    path.join(os.tmpdir(), "centaeris-electron-window-smoke-"),
  );
  const runtimeDataDir = path.join(tempRoot, "runtime-data");
  await fs.mkdir(runtimeDataDir, { recursive: true });
  let runtimeCleanupWaited = false;

  try {
    const firstLaunch = await runPackagedWindow({
      runtimeDataDir,
      userDataDir: path.join(tempRoot, "electron-user-data-1"),
      expression: FIRST_LAUNCH_EXPRESSION.replace(
        "__WORKSPACE_ROOT__",
        JSON.stringify(tempRoot),
      ),
      checkedLabel: "first launch",
      verifySingleInstance: true,
    });

    assertRecord(firstLaunch.createdSession, "session/new result");
    if (!firstLaunch.createdSession.id) {
      fail("session/new did not return an id");
    }
    assertRecord(firstLaunch.loadedSession, "session/load result");
    if (firstLaunch.loadedSession.id !== firstLaunch.createdSession.id) {
      fail("session/load did not reload the created session");
    }
    assertRecord(firstLaunch.savedConfig, "agent_runtime_config_set result");
    if (firstLaunch.savedConfig.toolParallelism !== 7) {
      fail("agent_runtime_config_set did not persist toolParallelism=7");
    }
    if (
      firstLaunch.runtimeConfigChanges?.length !== 1 ||
      !firstLaunch.runtimeConfigChanges[0] ||
      Array.isArray(firstLaunch.runtimeConfigChanges[0]) ||
      Object.keys(firstLaunch.runtimeConfigChanges[0]).length !== 0
    ) {
      fail("Electron did not project one exact runtime/config-changed event");
    }

    const secondLaunch = await runPackagedWindow({
      runtimeDataDir,
      userDataDir: path.join(tempRoot, "electron-user-data-2"),
      expression: SECOND_LAUNCH_EXPRESSION.replace(
        "__WORKSPACE_ROOT__",
        JSON.stringify(tempRoot),
      ).replace("__SESSION_ID__", JSON.stringify(firstLaunch.createdSession.id)),
      checkedLabel: "second launch",
      exitMode: "native",
    });

    if (
      !secondLaunch.sessions.some(
        (session) => session.id === firstLaunch.createdSession.id,
      )
    ) {
      fail("reopened Electron app did not restore the created session");
    }
    if (secondLaunch.config.toolParallelism !== 7) {
      fail("reopened Electron app did not restore toolParallelism=7");
    }

    await delay(RUNTIME_SERVER_IDLE_CLEANUP_MS);
    runtimeCleanupWaited = true;
    await assertRuntimeServerExited(runtimeDataDir);

    console.log(
      JSON.stringify(
        {
          ok: true,
          electronExe,
          tempRoot,
          checked: {
            renderer: true,
            bridge: true,
            nativeWindowChrome: true,
            thinWorkspaceShell: true,
            singleInstanceActivation: true,
            sessionCreateLoad: firstLaunch.createdSession.id,
            runtimeConfigReadWrite: true,
            reopenPersistence: true,
            headlessProjection: secondLaunch.projectionKinds,
            runtimeOps: true,
            schedulerStatus: true,
            commandFailures: firstLaunch.commandFailures.map(
              (failure) => failure.command,
            ),
            gracefulShutdown: true,
            nativeAppQuit: true,
            runtimeServerIdleExit: true,
          },
        },
        null,
        2,
      ),
    );
  } finally {
    if (!runtimeCleanupWaited) {
      await delay(RUNTIME_SERVER_IDLE_CLEANUP_MS);
    }
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
};

await main();

// Node's Windows CDP socket can remain in FIN_WAIT_2 after Electron exits.
process.exit(0);
