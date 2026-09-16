import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { createWslRuntimeBridge } from "../src/wslRuntimeBridge.mjs";
import { createRuntimeHostTransport } from "../src/runtimeHostTransport.mjs";
import { runtimeArtifactPath } from "../src/runtimeArtifact.mjs";

const repo = path.resolve(import.meta.dirname, "../../..");
const distribution = process.env.CENTAERIS_WSL_DISTRIBUTION || "Ubuntu-24.04";
const executablePath = process.env.CENTAERIS_RUNTIME_EXE || runtimeArtifactPath(repo, "release");
const { stdout: home } = await promisify(execFile)("wsl.exe", ["--distribution", distribution, "--exec", "printenv", "HOME"], { windowsHide: true });
const root = `${home.trim()}/centaeris-tui-smoke-${randomBytes(3).toString("hex")}`;
const environment = { ...process.env, CENTAERIS_RUNTIME_EXE: executablePath, CENTAERIS_WSL_DATA_DIR: `${root}/data`, CENTAERIS_TUI_SMOKE_WORKSPACE: `${root}/workspace 测试` };
const bridge = createWslRuntimeBridge({ executablePath, distribution, environment });
const transport = createRuntimeHostTransport({ executablePath, cwd: repo, bridge,
  emitHostEvent() {}, showFailureDialog(message) { throw new Error(message); },
  isAppReady: () => false, isQuitting: () => false,
});
try {
  const descriptor = await transport.invokeCommand("initialize", { request: { clientKind: "desktop", viewerId: "desktop-tui-coexistence" } });
  assert.equal(descriptor.status, "ok");
  await new Promise((resolve, reject) => {
    const child = spawn("cargo.exe", ["test", "--locked", "-p", "centaeris-tui", "wsl_runtime_keeps_other_clients_and_persists_sessions", "--", "--ignored", "--nocapture"], { cwd: repo, env: environment, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    for (const stream of [child.stdout, child.stderr]) stream.on("data", (chunk) => { output = `${output}${chunk}`.slice(-16000); });
    child.once("error", reject);
    child.once("close", (code) => code === 0 ? resolve() : reject(new Error(`TUI WSL acceptance failed (${code}): ${output}`)));
  });
  const sessions = await transport.invokeCommand("session/list", { request: {} });
  assert.ok(JSON.stringify(sessions).includes("WSL TUI persistence"), "Desktop sees the TUI session after TUI disconnects");
  console.log("Desktop/TUI share one WSL Runtime; TUI disconnect, session persistence and sandboxed sidecar acceptance passed");
} finally {
  await transport.requestAppExit();
  await delay(7000);
  await fs.rm(bridge.toDesktopPath(root), { recursive: true, force: true });
}
