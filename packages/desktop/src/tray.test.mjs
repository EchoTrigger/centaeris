import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import { fileURLToPath } from "node:url";
import test, { mock } from "node:test";

const probeMarker = "CENTAERIS_TRAY_REJECTION_PROBE=";

if (process.argv.includes("--rejection-probe")) {
  let menuTemplate = [];

  class FakeTray extends EventEmitter {
    setToolTip() {}

    setContextMenu() {}
  }

  mock.module("electron", {
    namedExports: {
      Menu: {
        buildFromTemplate(template) {
          menuTemplate = template;
          return template;
        },
      },
      Tray: FakeTray,
    },
  });

  const observedRejections = [];
  const reportedFailures = [];
  process.on("unhandledRejection", (error) => {
    observedRejections.push(error instanceof Error ? error.message : String(error));
  });

  let showMainWindowCalls = 0;
  const { createTrayController } = await import("./tray.mjs?rejection-probe");
  const trayController = createTrayController({
    trayIconPath: "probe.ico",
    showMainWindow: () => {
      showMainWindowCalls += 1;
      if (showMainWindowCalls === 1) {
        throw new Error("show-main-window-threw");
      }
      return Promise.reject(new Error("show-main-window-rejected"));
    },
    emitHostEvent: () => {},
    requestAppExit: () => Promise.reject(new Error("request-app-exit-failed")),
    reportActionFailure: (actionName, error) => {
      reportedFailures.push([
        actionName,
        error instanceof Error ? error.message : String(error),
      ]);
    },
  });
  const tray = trayController.ensureTray();

  menuTemplate.find(({ label }) => label === "New Chat").click();
  menuTemplate.find(({ label }) => label === "Open Centaeris").click();
  menuTemplate.find(({ label }) => label === "Exit").click();
  tray.emit("double-click");

  await new Promise((resolve) => setImmediate(resolve));
  process.stdout.write(`${probeMarker}${JSON.stringify({
    observedRejections: observedRejections.sort(),
    reportedFailures,
  })}\n`);
} else {
  test("tray actions handle rejected host operations", () => {
    const result = spawnSync(
      process.execPath,
      [
        "--experimental-test-module-mocks",
        fileURLToPath(import.meta.url),
        "--rejection-probe",
      ],
      {
        cwd: import.meta.dirname,
        encoding: "utf8",
      },
    );

    assert.equal(result.status, 0, result.stderr);
    const probeLine = result.stdout
      .split(/\r?\n/u)
      .find((line) => line.startsWith(probeMarker));
    assert.ok(probeLine, "tray rejection probe did not report its result");
    const probeResult = JSON.parse(probeLine.slice(probeMarker.length));
    assert.deepEqual(probeResult.observedRejections, []);
    assert.deepEqual(probeResult.reportedFailures, [
      ["new-chat", "show-main-window-threw"],
      ["open", "show-main-window-rejected"],
      ["exit", "request-app-exit-failed"],
      ["double-click", "show-main-window-rejected"],
    ]);
  });
}
