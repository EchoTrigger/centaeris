import assert from "node:assert/strict";
import test from "node:test";
import {
  createTrustedRendererPolicy,
  requireExternalHttpsUrl,
  requireTrustedRendererEvent,
  requireTrustedRendererUrl,
} from "./rendererSecurity.mjs";

const policy = createTrustedRendererPolicy({
  filePath: "D:/Centaeris/ui-dist/index.html",
  devServerUrl: "http://127.0.0.1:5173",
});

test("trusted renderer accepts only its exact file or loopback dev URL", () => {
  assert.equal(requireTrustedRendererUrl(policy.fileUrl, policy), policy.fileUrl);
  assert.equal(
    requireTrustedRendererUrl("http://127.0.0.1:5173", policy),
    "http://127.0.0.1:5173/",
  );
  for (const url of [
    "https://example.com",
    "file:///D:/banana.html",
    `${policy.fileUrl}#chat`,
    "http://127.0.0.1:5173/chat",
    "http://localhost:5173",
  ]) {
    assert.throws(() => requireTrustedRendererUrl(url, policy), /untrusted renderer/);
  }
});

test("host IPC requires exact webContents and main frame", () => {
  const mainFrame = { url: policy.fileUrl };
  const webContents = { mainFrame };
  const mainWindow = { isDestroyed: () => false, webContents };
  assert.doesNotThrow(() => requireTrustedRendererEvent({ sender: webContents, senderFrame: mainFrame }, mainWindow, policy));
  assert.throws(
    () => requireTrustedRendererEvent({ sender: { mainFrame }, senderFrame: mainFrame }, mainWindow, policy),
    /sender is not/,
  );
  assert.throws(
    () => requireTrustedRendererEvent({ sender: webContents, senderFrame: { url: policy.fileUrl } }, mainWindow, policy),
    /main frame/,
  );
});

test("external content accepts only credential-free HTTPS", () => {
  assert.equal(requireExternalHttpsUrl("https://example.com/path"), "https://example.com/path");
  for (const url of [
    "http://example.com",
    "mailto:test@example.com",
    "file:///D:/banana.txt",
    "data:text/html,banana",
    "javascript:alert(1)",
    "banana://example.com",
    "https://user:secret@example.com",
  ]) {
    assert.throws(() => requireExternalHttpsUrl(url), /only supports/);
  }
});
