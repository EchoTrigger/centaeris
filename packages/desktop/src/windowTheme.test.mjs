import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import { fileURLToPath } from "node:url";
import test, { mock } from "node:test";

if (process.argv.includes("--theme-probe")) {
  let window;
  class FakeWindow extends EventEmitter {
    constructor() {
      super(); window = this; this.overlays = [];
      this.webContents = new EventEmitter();
      this.webContents.setWindowOpenHandler = () => {};
    }
    isDestroyed() { return false; }
    setTitleBarOverlay(value) { this.overlays.push(value); }
    async loadURL() {}
    async loadFile() {}
  }
  mock.module("electron", { namedExports: {
    app: { isPackaged: false }, BrowserWindow: FakeWindow,
    nativeTheme: { shouldUseDarkColors: false }, session: {}, WebContentsView: class {},
  } });
  const { createWindowShell } = await import("./windowShell.mjs");
  const shell = createWindowShell({ preloadPath: "preload.cjs", packagedUiDistIndex: "missing.html", devUiDistIndex: "missing.html", uiDevServerUrl: "http://localhost:5117" });
  await shell.createMainWindow();
  for (const color of ["#242424", "#F5F5F4", "#f5f5f4", null, "#ff0000"]) window.webContents.emit("did-change-theme-color", {}, color);
  assert.deepEqual(window.overlays, [
    { color: "#242424", symbolColor: "#ededed", height: 36 },
    { color: "#f5f5f4", symbolColor: "#202428", height: 36 },
    { color: "#f5f5f4", symbolColor: "#202428", height: 36 },
  ]);
} else {
  test("Windows caption buttons follow dark and light Chromium theme events", { skip: process.platform !== "win32" }, () => {
    const result = spawnSync(process.execPath, ["--experimental-test-module-mocks", fileURLToPath(import.meta.url), "--theme-probe"], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  });
}
