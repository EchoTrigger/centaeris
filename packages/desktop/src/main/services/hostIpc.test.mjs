import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test, { mock } from "node:test";

if (process.argv.includes("--preview-ipc-probe")) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-preview-ipc-"));
  try {
    const handlers = new Map();
    mock.module("electron", { namedExports: {
      ipcMain: { handle: (name, handler) => handlers.set(name, handler) },
      nativeTheme: {},
    } });
    const { registerHostIpc } = await import("./hostIpc.mjs");
    const { requireHostCommand } = await import("../hostContract.mjs");
    const trusted = {};
    const delegated = [];
    let shuttingDown = false;
    registerHostIpc({
      invokeRustHostCommand: async (...args) => {
        delegated.push(args);
        return { delegated: true };
      },
      requestAppExit: async () => {},
      isShuttingDown: () => shuttingDown,
      localShellActions: {},
      validateTrustedRendererEvent: (event) => {
        assert.equal(event, trusted, "preview requires the trusted renderer");
      },
      previewCwd: root,
    });
    await fs.writeFile(path.join(root, "sample.txt"), "local preview");
    const invoke = handlers.get("host:invoke");
    const request = {
      command: "desktop_file_preview_read",
      payload: { request: { path: "sample.txt", basePath: "." } },
    };
    const response = await invoke(trusted, request);
    assert.equal(response.content, "local preview");
    assert.equal(requireHostCommand(request.command).local, true);
    assert.equal(delegated.length, 0);
    await assert.rejects(invoke({}, request), /preview requires the trusted renderer/);
    await assert.rejects(invoke(trusted, {
      ...request,
      payload: { request: { path: "sample.txt", unknown: true } },
    }), { code: "invalid_request" });
    await assert.rejects(invoke(trusted, {
      ...request,
      payload: { ...request.payload, authorization: {} },
    }), (error) => error.code === "invalid_request" && error.payload.code === -32600);
    shuttingDown = true;
    await assert.rejects(invoke(trusted, request), /Centaeris is shutting down/);
    shuttingDown = false;
    assert.deepEqual(await invoke(trusted, { command: "session/list", payload: {} }), { delegated: true });
    assert.equal(delegated.length, 1);
    assert.equal(delegated[0][0], "session/list");
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
} else {
  test("trusted Desktop preview IPC reads locally and preserves request and shutdown boundaries", () => {
    const result = spawnSync(process.execPath, [
      "--experimental-test-module-mocks", fileURLToPath(import.meta.url), "--preview-ipc-probe",
    ], { encoding: "utf8", timeout: 10_000 });
    assert.equal(result.status, 0, `${result.stdout}${result.stderr}`);
  });
}
