import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import path from "node:path";
import { runtimeArtifactPath } from "../src/runtimeArtifact.mjs";
import { createWslRuntimeBridge } from "../src/wslRuntimeBridge.mjs";
import { createRuntimeHostTransport } from "../src/runtimeHostTransport.mjs";

const executablePath = process.env.CENTAERIS_RUNTIME_EXE
  || runtimeArtifactPath(path.resolve(import.meta.dirname, "../../.."), "release");
const data = `/tmp/centaeris-wsl-test-${process.pid}-${Date.now()}`;
const workspace = `${data}-workspace`;
const distribution = process.env.CENTAERIS_WSL_DISTRIBUTION || "Ubuntu-24.04";
const bridge = createWslRuntimeBridge({
  executablePath,
  distribution,
  environment: { ...process.env, CENTAERIS_WSL_DATA_DIR: data },
});
let connected;
const connect = bridge.connect;
bridge.connect = async (...args) => { connected = await connect(...args); return connected; };
const transport = createRuntimeHostTransport({
  executablePath, cwd: process.cwd(), bridge,
  emitHostEvent() {}, showFailureDialog(message) { throw new Error(message); },
  isAppReady: () => false, isQuitting: () => false,
});
try {
  const init = { request: { clientKind: "desktop", viewerId: "wsl-smoke" } };
  const descriptor = await transport.invokeCommand("initialize", init);
  assert.equal(descriptor.status, "ok");
  const before = await transport.invokeCommand("workspace_get", {});
  connected.destroy();
  const after = await transport.invokeCommand("workspace_get", {});
  assert.deepEqual(after, before);
  const localWorkspace = bridge.toDesktopPath(workspace);
  await fs.mkdir(localWorkspace);
  await transport.invokeCommand("workspace_activate", { request: { root: workspace } });
  const start = async () => {
    await fs.rm(`${localWorkspace}\\ready`, { force: true });
    const response = await transport.invokeCommand("sidecar_start", { request: {
      command: "python3", workspaceRoot: workspace, args: ["-c",
        "import os,time,pathlib,sys\ntry:\n pathlib.Path(sys.argv[1]).read_text(); pathlib.Path('leaked').touch()\nexcept PermissionError: pass\nif os.fork()==0:\n os.setsid(); pathlib.Path('ready').touch(); time.sleep(2); pathlib.Path('escaped').touch()\nelse: time.sleep(30)",
        `${data}/config.toml`],
    } });
    for (let i = 0; i < 100; i += 1) {
      if (await fs.access(`${localWorkspace}\\ready`).then(() => true, () => false)) return response;
      await delay(25);
    }
    throw new Error("Sidecar did not become ready");
  };
  const sidecar = await start();
  await transport.invokeCommand("sidecar_stop", { request: { sidecarId: sidecar.sidecarId } });
  await delay(2200);
  assert.equal(await fs.access(`${localWorkspace}\\escaped`).then(() => true, () => false), false);
  assert.equal(await fs.access(`${localWorkspace}\\leaked`).then(() => true, () => false), false);
  await start();
  const unit = `centaeris-runtime-${createHash("sha256").update(data).digest("hex").slice(0, 16)}`;
  await promisify(execFile)("wsl.exe", ["--distribution", distribution, "--exec", "systemctl", "--user",
    "kill", "--kill-whom=main", "--signal=KILL", unit], { windowsHide: true, timeout: 10000 });
  await delay(2200);
  assert.equal(await fs.access(`${localWorkspace}\\escaped`).then(() => true, () => false), false);
  await transport.invokeCommand("workspace_get", {});
  console.log("WSL deployment, initialize/build identity, reconnect, sidecar isolation/stop and Runtime crash cleanup passed");
} finally {
  await transport.requestAppExit();
  // The server owns its shutdown; wait for the relay EOF before removing this
  // test's newly created profile. Never touch the user's real profile.
  if (connected && !connected.destroyed) {
    await new Promise((resolve) => { connected.once("close", resolve); setTimeout(resolve, 5000).unref(); });
  }
  await delay(7000);
  await fs.rm(bridge.toDesktopPath(data), { recursive: true, force: true });
  await fs.rm(bridge.toDesktopPath(workspace), { recursive: true, force: true });
}
