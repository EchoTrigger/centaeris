import { app, BrowserWindow, session, WebContentsView } from "electron";
import fs from "node:fs";
import {
  createTrustedRendererPolicy,
  requireExternalHttpsUrl,
  requireTrustedRendererEvent,
  requireTrustedRendererUrl,
} from "./rendererSecurity.mjs";

const EXTERNAL_CONTENT_PARTITION = "centaeris-external-content";
const NATIVE_TITLEBAR_HEIGHT = 36;

export const createWindowShell = ({
  preloadPath,
  packagedUiDistIndex,
  devUiDistIndex,
  uiDevServerUrl,
  shouldCloseDirectly,
  onCloseToTray,
}) => {
  if (!preloadPath) {
    throw new Error("preloadPath is required");
  }
  if (!uiDevServerUrl) {
    throw new Error("uiDevServerUrl is required");
  }

  let mainWindow = null;
  let trustedRendererPolicy = null;
  let externalContentView = null;

  const reportSecurityRejection = (error) => {
    console.error(`renderer_security_rejected: ${error.message}`);
  };

  const closeExternalContentView = () => {
    if (!externalContentView) return;
    if (mainWindow && !mainWindow.isDestroyed()) {
      mainWindow.contentView.removeChildView(externalContentView);
    }
    if (!externalContentView.webContents.isDestroyed()) {
      externalContentView.webContents.close({ waitForBeforeUnload: false });
    }
    externalContentView = null;
  };

  const resizeExternalContentView = () => {
    if (!mainWindow || !externalContentView) return;
    const [width, height] = mainWindow.getContentSize();
    externalContentView.setBounds({
      x: 0,
      y: NATIVE_TITLEBAR_HEIGHT,
      width,
      height: Math.max(1, height - NATIVE_TITLEBAR_HEIGHT),
    });
  };

  const openExternalContent = async (rawUrl) => {
    const url = requireExternalHttpsUrl(rawUrl);
    if (!mainWindow || mainWindow.isDestroyed()) {
      throw new Error("main window is unavailable for external HTTPS content");
    }
    if (!externalContentView) {
      const isolatedSession = session.fromPartition(EXTERNAL_CONTENT_PARTITION, { cache: false });
      isolatedSession.setPermissionRequestHandler((_webContents, _permission, callback) => callback(false));
      isolatedSession.setPermissionCheckHandler(() => false);
      isolatedSession.on("will-download", (event) => event.preventDefault());
      externalContentView = new WebContentsView({
        webPreferences: {
          partition: EXTERNAL_CONTENT_PARTITION,
          sandbox: true,
          contextIsolation: true,
          nodeIntegration: false,
          webSecurity: true,
        },
      });
      externalContentView.webContents.on("will-navigate", (event, targetUrl) => {
        try {
          requireExternalHttpsUrl(targetUrl);
        } catch (error) {
          event.preventDefault();
          reportSecurityRejection(error);
        }
      });
      externalContentView.webContents.on("will-attach-webview", (event) => event.preventDefault());
      externalContentView.webContents.on("before-input-event", (_event, input) => {
        if (input.type === "keyDown" && input.key === "Escape") closeExternalContentView();
      });
      externalContentView.webContents.setWindowOpenHandler(({ url: targetUrl }) => {
        try {
          const nextUrl = requireExternalHttpsUrl(targetUrl);
          void externalContentView?.webContents.loadURL(nextUrl).catch(reportSecurityRejection);
        } catch (error) {
          reportSecurityRejection(error);
        }
        return { action: "deny" };
      });
      mainWindow.contentView.addChildView(externalContentView);
      resizeExternalContentView();
    }
    await externalContentView.webContents.loadURL(url);
    externalContentView.webContents.focus();
  };

  const createMainWindow = async () => {
    const platformWindowOptions =
      process.platform === "darwin"
        ? { titleBarStyle: "hiddenInset" }
        : process.platform === "win32"
          ? {
              titleBarStyle: "hidden",
              titleBarOverlay: {
                color: "#f7f7f7",
                symbolColor: "#202124",
                height: 36,
              },
            }
          : {};
    mainWindow = new BrowserWindow({
      width: 1180,
      height: 820,
      minWidth: 940,
      minHeight: 620,
      ...platformWindowOptions,
      show: false,
      title: "Centaeris",
      backgroundColor: "#edf2f8",
      webPreferences: {
        preload: preloadPath,
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
        backgroundThrottling: false,
      },
    });

    const packagedDistExists = fs.existsSync(packagedUiDistIndex);
    const devDistExists = fs.existsSync(devUiDistIndex);
    const distIndexPath = packagedDistExists ? packagedUiDistIndex : devUiDistIndex;
    trustedRendererPolicy = createTrustedRendererPolicy({
      filePath: distIndexPath,
      devServerUrl: uiDevServerUrl,
    });
    mainWindow.webContents.on("will-navigate", (event, targetUrl) => {
      try {
        requireTrustedRendererUrl(targetUrl, trustedRendererPolicy);
      } catch (error) {
        event.preventDefault();
        reportSecurityRejection(error);
      }
    });
    mainWindow.webContents.on("will-attach-webview", (event) => event.preventDefault());
    let rendererRecoveryPending = false;
    mainWindow.webContents.on("did-finish-load", () => {
      rendererRecoveryPending = false;
    });
    mainWindow.webContents.on("render-process-gone", (_event, details) => {
      console.error(`desktop renderer stopped: ${details.reason}`);
      if (rendererRecoveryPending || shouldCloseDirectly?.()) return;
      rendererRecoveryPending = true;
      mainWindow?.reload();
    });
    mainWindow.webContents.setWindowOpenHandler(({ url }) => {
      try {
        const targetUrl = requireExternalHttpsUrl(url);
        void openExternalContent(targetUrl).catch(reportSecurityRejection);
      } catch (error) {
        reportSecurityRejection(error);
      }
      return { action: "deny" };
    });
    mainWindow.on("resize", resizeExternalContentView);

    mainWindow.on("close", (event) => {
      if (shouldCloseDirectly?.()) {
        return;
      }
      event.preventDefault();
      void Promise.resolve(onCloseToTray?.()).catch((error) => {
        console.error(`failed to detach viewer tasks: ${error.message}`);
      });
      mainWindow?.hide();
    });

    mainWindow.on("closed", () => {
      closeExternalContentView();
      mainWindow = null;
      trustedRendererPolicy = null;
    });

    mainWindow.once("ready-to-show", () => {
      mainWindow?.show();
    });

    const shouldLoadDist =
      process.env.CENTAERIS_ELECTRON_LOAD_DIST === "1" || packagedDistExists || devDistExists;
    if (shouldLoadDist) {
      await mainWindow.loadFile(distIndexPath);
      return;
    }
    if (app.isPackaged) {
      throw new Error(
        `UI dist is required in packaged mode: ${packagedUiDistIndex}`,
      );
    }

    await mainWindow.loadURL(uiDevServerUrl);
  };

  const validateTrustedRendererEvent = (event) => {
    requireTrustedRendererEvent(event, mainWindow, trustedRendererPolicy);
  };

  const getRequestWindow = (event) => {
    validateTrustedRendererEvent(event);
    return mainWindow;
  };

  const showMainWindow = async () => {
    await app.whenReady();
    if (!mainWindow || mainWindow.isDestroyed()) {
      await createMainWindow();
    }
    if (mainWindow?.isMinimized()) {
      mainWindow.restore();
    }
    mainWindow?.show();
    mainWindow?.focus();
  };

  const emitHostEvent = (eventName, payload) => {
    if (!mainWindow || mainWindow.isDestroyed()) return;
    mainWindow.webContents.send("host:event", {
      eventName,
      payload,
    });
  };

  return {
    createMainWindow,
    showMainWindow,
    emitHostEvent,
    getMainWindow: () => mainWindow,
    getRequestWindow,
    validateTrustedRendererEvent,
    hasOpenWindows: () => BrowserWindow.getAllWindows().length > 0,
  };
};
