import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import { fileURLToPath } from "node:url";
import test, { mock } from "node:test";

if (process.argv.includes("--lifecycle-probe")) {
  const windows = [];
  let finishLoad;
  class FakeWindow extends EventEmitter {
    constructor() {
      super(); windows.push(this);
      this.webContents = new EventEmitter();
      this.webContents.mainFrame = { url: "http://localhost:5117/" };
      this.webContents.setWindowOpenHandler = () => {};
    }
    isDestroyed() { return false; }
    isMinimized() { return false; }
    show() {}
    focus() {}
    loadURL() { return new Promise((resolve) => { finishLoad = resolve; }); }
  }
  mock.module("electron", { namedExports: {
    app: { isPackaged: false, whenReady: async () => {} }, BrowserWindow: FakeWindow,
    nativeTheme: { shouldUseDarkColors: false }, screen: { getPrimaryDisplay: () => ({ workAreaSize: { width: 1920, height: 1080 } }) }, session: {}, WebContentsView: class {},
  } });
  const { createWindowShell } = await import("./windowShell.mjs");
  const shell = createWindowShell({ preloadPath: "preload.cjs", packagedUiDistIndex: "missing.html", devUiDistIndex: "missing.html", uiDevServerUrl: "http://localhost:5117" });
  // A second launch can arrive while startup is waiting for the Runtime.
  const earlyShow = shell.showMainWindow();
  await new Promise((resolve) => setImmediate(resolve));
  const startupCreate = shell.createMainWindow();
  assert.equal(windows.length, 1, "startup must reuse the window opened by second-instance");
  finishLoad();
  await Promise.all([earlyShow, startupCreate]);
  await shell.createMainWindow();
  assert.equal(windows.length, 1);
  const owner = windows[0].webContents;
  assert.doesNotThrow(() => shell.validateTrustedRendererEvent({ sender: owner, senderFrame: owner.mainFrame }));
  assert.throws(() => shell.validateTrustedRendererEvent({ sender: {}, senderFrame: owner.mainFrame }), /not the trusted renderer/);
} else {
  test("startup and second-instance retain one trusted renderer", () => {
    const result = spawnSync(process.execPath, ["--experimental-test-module-mocks", fileURLToPath(import.meta.url), "--lifecycle-probe"], { encoding: "utf8", timeout: 10000 });
    assert.equal(result.status, 0, result.stderr);
  });
}
