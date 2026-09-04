import { pathToFileURL } from "node:url";

export const createTrustedRendererPolicy = ({ filePath, devServerUrl }) => {
  const fileUrl = pathToFileURL(filePath).href;
  const devUrl = new URL(devServerUrl);
  if (
    devUrl.protocol !== "http:"
    || !["127.0.0.1", "localhost", "[::1]"].includes(devUrl.hostname)
    || devUrl.username
    || devUrl.password
    || devUrl.search
    || devUrl.hash
  ) {
    throw new Error("trusted renderer dev server must be an exact loopback HTTP URL");
  }
  return Object.freeze({ fileUrl, devUrl: devUrl.href });
};

export const requireTrustedRendererUrl = (rawUrl, policy) => {
  const url = new URL(rawUrl);
  if (url.protocol === "file:") {
    if (url.href !== policy.fileUrl) {
      throw new Error(`untrusted renderer file URL: ${url.href}`);
    }
    return url.href;
  }
  if (url.href !== policy.devUrl) {
    throw new Error(`untrusted renderer URL: ${url.href}`);
  }
  return url.href;
};

export const requireTrustedRendererEvent = (event, mainWindow, policy) => {
  if (!mainWindow || mainWindow.isDestroyed()) {
    throw new Error("trusted renderer window is unavailable");
  }
  if (event?.sender !== mainWindow.webContents) {
    throw new Error("host IPC sender is not the trusted renderer");
  }
  if (event?.senderFrame !== mainWindow.webContents.mainFrame) {
    throw new Error("host IPC sender frame is not the trusted main frame");
  }
  requireTrustedRendererUrl(event.senderFrame.url, policy);
};

export const requireExternalHttpsUrl = (rawUrl) => {
  const url = new URL(rawUrl);
  if (
    url.protocol !== "https:"
    || !url.hostname
    || url.username
    || url.password
  ) {
    throw new Error(`external navigation only supports credential-free HTTPS: ${url.href}`);
  }
  return url.href;
};
