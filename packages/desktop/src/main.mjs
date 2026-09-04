import { app, dialog } from "electron";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { requireHostCommand } from "./hostContract.mjs";
import { registerHostIpc } from "./hostIpc.mjs";
import { createLocalShellActions } from "./localShellActions.mjs";
import { createRuntimeHostTransport } from "./runtimeHostTransport.mjs";
import { createTrayController } from "./tray.mjs";
import { createWindowShell } from "./windowShell.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const HOST_ROOT = path.resolve(__dirname, "..");
const REPO_ROOT = path.resolve(HOST_ROOT, "..", "..");
const PACKAGED_UI_DIST_INDEX = path.join(
  process.resourcesPath,
  "ui-dist",
  "index.html",
);
const DEV_UI_DIST_INDEX = path.join(REPO_ROOT, "packages", "ui", "dist", "index.html");
const PRELOAD_PATH = path.join(__dirname, "preload.cjs");

const UI_DEV_SERVER_URL =
  process.env.CENTAERIS_UI_DEV_SERVER_URL || "http://127.0.0.1:5173";
const RUNTIME_EXE =
  process.env.CENTAERIS_RUNTIME_EXE ||
  (app.isPackaged
    ? path.join(process.resourcesPath, "bin", "centaeris-runtime.exe")
    : path.join(REPO_ROOT, "target", "debug", "centaeris-runtime.exe"));
const RUNTIME_CWD = app.isPackaged ? path.dirname(RUNTIME_EXE) : REPO_ROOT;
const TRAY_ICON_FILE = process.platform === "darwin" ? "icon.icns" : "icon.ico";
const TRAY_ICON_PATH = app.isPackaged
  ? path.join(process.resourcesPath, TRAY_ICON_FILE)
  : path.join(HOST_ROOT, "assets", TRAY_ICON_FILE);
const IS_SMOKE_RUN = process.env.CENTAERIS_ELECTRON_SMOKE === "1";
let isQuitting = false;
let appExitStarted = false;
let runtimeHostTransport = null;

const windowShell = createWindowShell({
  preloadPath: PRELOAD_PATH,
  packagedUiDistIndex: PACKAGED_UI_DIST_INDEX,
  devUiDistIndex: DEV_UI_DIST_INDEX,
  uiDevServerUrl: UI_DEV_SERVER_URL,
  shouldCloseDirectly: () => isQuitting || runtimeHostTransport?.isShuttingDown() === true,
  onCloseToTray: () =>
    runtimeHostTransport.invokeCommand(
      "_centaeris/session/agent-runs/detach-viewer",
      { request: { viewerId: "desktop-main" } },
      requireHostCommand("_centaeris/session/agent-runs/detach-viewer"),
    ),
});

runtimeHostTransport = createRuntimeHostTransport({
  executablePath: RUNTIME_EXE,
  cwd: RUNTIME_CWD,
  emitHostEvent: windowShell.emitHostEvent,
  isAppReady: () => app.isReady(),
  isQuitting: () => isQuitting,
  isSmokeRun: IS_SMOKE_RUN,
  showFailureDialog: (message, stderrTail) => {
    dialog.showErrorBox(
      "Centaeris Runtime stopped",
      `${message}${stderrTail ? `\n\n${stderrTail}` : ""}`,
    );
  },
});

const localShellActions = createLocalShellActions({
  getRequestWindow: windowShell.getRequestWindow,
  getMainWindow: windowShell.getMainWindow,
  getHomePath: () => app.getPath("home"),
  invokeRustHostCommand: runtimeHostTransport.invokeCommand,
});

const requestAppExit = async () => {
  if (appExitStarted) {
    return;
  }
  appExitStarted = true;
  isQuitting = true;
  try {
    await runtimeHostTransport.requestAppExit();
  } finally {
    app.quit();
  }
};

const initializeRuntimeHost = () =>
  runtimeHostTransport.invokeCommand(
    "initialize",
    {
      request: {
        clientKind: "desktop",
        viewerId: "desktop-main",
      },
    },
  );

const trayController = createTrayController({
  trayIconPath: TRAY_ICON_PATH,
  showMainWindow: windowShell.showMainWindow,
  emitHostEvent: windowShell.emitHostEvent,
  requestAppExit,
});

const showMainWindowSafely = () => {
  void windowShell.showMainWindow().catch((error) => {
    console.error(`failed to show Centaeris window: ${error.stack ?? error.message}`);
    dialog.showErrorBox("Centaeris window unavailable", error.message);
  });
};

const invokeRendererHostCommand = async (command, payload, metadata) => {
  return runtimeHostTransport.invokeCommand(command, payload, metadata);
};

registerHostIpc({
  invokeRustHostCommand: invokeRendererHostCommand,
  requestAppExit,
  isShuttingDown: runtimeHostTransport.isShuttingDown,
  localShellActions,
  validateTrustedRendererEvent: windowShell.validateTrustedRendererEvent,
});

const hasSingleInstanceLock = app.requestSingleInstanceLock();

if (!hasSingleInstanceLock) {
  app.quit();
} else {
  app.on("second-instance", () => {
    showMainWindowSafely();
  });

  app.whenReady().then(async () => {
    await initializeRuntimeHost();
    await windowShell.createMainWindow();
    trayController.ensureTray();

    app.on("activate", () => {
      if (!windowShell.hasOpenWindows()) {
        showMainWindowSafely();
        return;
      }
      showMainWindowSafely();
    });
  }).catch((error) => {
    console.error(`Centaeris Runtime initialization failed: ${error.stack ?? error.message}`);
    dialog.showErrorBox("Centaeris Runtime unavailable", error.message);
    app.quit();
  });
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin" && isQuitting) {
    app.quit();
  }
});

app.on("before-quit", (event) => {
  isQuitting = true;
  if (appExitStarted) {
    return;
  }
  event.preventDefault();
  void requestAppExit();
});
