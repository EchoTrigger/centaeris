import { app, BrowserWindow, nativeTheme, screen } from "electron";
import {
  createTrustedRendererPolicy,
  requireTrustedRendererEvent,
  requireTrustedRendererUrl,
} from "./rendererSecurity.mjs";

export function createWindowShell({ rendererPath, preloadPath, devServerUrl }) {
  const policy = createTrustedRendererPolicy({
    filePath: rendererPath,
    devServerUrl: app.isPackaged ? undefined : devServerUrl,
  });
  let window;
  let opening;

  async function createWindow() {
    const { width, height } = screen.getPrimaryDisplay().workAreaSize;
    const next = new BrowserWindow({
      width: Math.min(1280, width),
      height: Math.min(800, height),
      show: false,
      backgroundColor: nativeTheme.shouldUseDarkColors ? "#242424" : "#f5f5f4",
      webPreferences: {
        preload: preloadPath,
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
      },
    });
    next.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
    next.webContents.on("will-attach-webview", (event) => event.preventDefault());
    const guardNavigation = (event, url) => {
      try {
        requireTrustedRendererUrl(url, policy);
      } catch {
        event.preventDefault();
      }
    };
    next.webContents.on("will-navigate", guardNavigation);
    next.webContents.on("will-redirect", guardNavigation);
    next.on("closed", () => {
      if (window === next) window = undefined;
    });
    let recoveryPending = false;
    next.webContents.on("did-finish-load", () => {
      recoveryPending = false;
    });
    next.webContents.on("render-process-gone", () => {
      if (recoveryPending || next.isDestroyed()) return;
      recoveryPending = true;
      next.reload();
    });

    window = next;
    try {
      if (policy.devUrl) {
        await next.loadURL(policy.devUrl);
      } else {
        await next.loadFile(rendererPath);
      }
    } catch (error) {
      if (!next.isDestroyed()) next.destroy();
      throw error;
    }
  }

  async function show() {
    await app.whenReady();
    if (opening) {
      await opening;
    } else if (!window || window.isDestroyed()) {
      opening = createWindow();
      try {
        await opening;
      } finally {
        opening = undefined;
      }
    }
    if (!window || window.isDestroyed()) return;
    if (window.isMinimized()) window.restore();
    window.show();
    window.focus();
  }

  return {
    show,
    validateRendererEvent: (event) => requireTrustedRendererEvent(event, window, policy),
  };
}

export async function startWindowShell(shell, onError = console.error) {
  if (!app.requestSingleInstanceLock()) {
    app.quit();
    return;
  }
  const show = () => void shell.show().catch(onError);
  app.on("second-instance", show);
  app.on("activate", show);
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") app.quit();
  });
  await shell.show();
}
