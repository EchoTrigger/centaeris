import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { spawn } from "node:child_process";

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

export async function waitFor(read, label) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    const value = await read();
    if (value) return value;
    await pause(50);
  }
  throw new Error(`Timed out waiting for ${label}`);
}

function connect(url) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const pending = new Map();
    let nextId = 0;
    socket.addEventListener("error", reject, { once: true });
    socket.addEventListener("message", event => {
      const message = JSON.parse(event.data);
      const entry = pending.get(message.id);
      if (!entry) return;
      clearTimeout(entry.timer); pending.delete(message.id);
      if (message.error) entry.reject(new Error(JSON.stringify(message.error)));
      else entry.resolve(message.result);
    });
    socket.addEventListener("close", () => {
      for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new Error("Chrome connection closed")); }
      pending.clear();
    });
    socket.addEventListener("open", () => resolve({
      send(method, params = {}, sessionId) {
        return new Promise((resolveCommand, rejectCommand) => {
          const id = ++nextId;
          const timer = setTimeout(() => { pending.delete(id); rejectCommand(new Error(`Chrome command timed out: ${method}`)); }, 10000);
          pending.set(id, { resolve: resolveCommand, reject: rejectCommand, timer });
          socket.send(JSON.stringify({ id, method, params, sessionId }));
        });
      },
      close: () => socket.close(),
    }), { once: true });
  });
}

// Real animation frames and keyboard events exercise focus. Device metrics,
// rather than --window-size, make narrow tests run at the requested CSS width.
export async function runBrowser(url, scenario, profile, inspect) {
  assert.ok(process.env.CHROME_BIN, "Set CHROME_BIN to a Chromium executable");
  const browser = spawn(process.env.CHROME_BIN, [
    "--headless", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    ...(process.platform === "win32" ? ["--no-sandbox"] : []),
    "--remote-debugging-port=0", "--remote-debugging-address=127.0.0.1",
    `--user-data-dir=${profile}`, "about:blank",
  ], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  let browserError;
  browser.on("error", error => { browserError = error; });
  let diagnostics = "";
  browser.stderr.on("data", chunk => { diagnostics = (diagnostics + chunk.toString()).slice(-4000); });
  const exited = new Promise(resolve => browser.once("exit", resolve));
  let client;
  try {
    const port = await waitFor(async () => {
      if (browserError) throw browserError;
      if (browser.exitCode !== null) throw new Error(`Chrome exited: ${diagnostics}`);
      try { return (await readFile(join(profile, "DevToolsActivePort"), "utf8")).split("\n"); }
      catch (error) { if (error.code === "ENOENT") return null; throw error; }
    }, "Chrome debugging endpoint");
    client = await connect(`ws://127.0.0.1:${port[0]}${port[1]}`);
    const browserVersion = await client.send("Browser.getVersion");
    const { targetId } = await client.send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await client.send("Target.attachToTarget", { targetId, flatten: true });
    const send = (method, params) => client.send(method, params, sessionId);
    const evaluate = async expression => {
      const response = await send("Runtime.evaluate", { expression: typeof expression === "function" ? `(${expression.toString()})()` : expression, returnByValue: true, awaitPromise: true });
      if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
      return response.result.value;
    };
    const frame = () => evaluate("new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(true))))");
    const press = async (key, code = key, modifiers = 0) => {
      const virtual = { Enter: 13, Tab: 9, Escape: 27, ArrowRight: 39, ArrowLeft: 37, Home: 36, End: 35 }[key];
      await send("Input.dispatchKeyEvent", { type: "keyDown", key, code, modifiers, windowsVirtualKeyCode: virtual, nativeVirtualKeyCode: virtual,
        ...(key === "Enter" ? { text: "\r", unmodifiedText: "\r" } : {}) });
      await send("Input.dispatchKeyEvent", { type: "keyUp", key, code, modifiers, windowsVirtualKeyCode: virtual });
      await frame();
    };
    const height = scenario.height ?? 900;
    await send("Page.enable");
    await send("Emulation.setDeviceMetricsOverride", { width: scenario.width, height, deviceScaleFactor: 1, mobile: false });
    await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: scenario.reduced ? "reduce" : "no-preference" }] });
    if (scenario.timeZone) await send("Emulation.setTimezoneOverride", { timezoneId: scenario.timeZone });
    if (scenario.locale) await send("Emulation.setLocaleOverride", { locale: scenario.locale });
    if (scenario.timeZone) assert.equal(await evaluate("new Intl.DateTimeFormat().resolvedOptions().timeZone"), scenario.timeZone, "browser timezone override is active");
    if (scenario.locale) assert.equal(await evaluate("new Intl.DateTimeFormat().resolvedOptions().locale"), scenario.locale, "browser locale override is active");
    await send("Page.navigate", { url });
    await send("Page.bringToFront");
    assert.deepEqual(await evaluate("({ width: innerWidth, height: innerHeight })"), { width: scenario.width, height }, "browser viewport matches requested CSS dimensions");
    return await inspect({ send, evaluate, frame, press, browserVersion });
  } finally {
    if (client) { await client.send("Browser.close").catch(() => {}); client.close(); }
    if (browser.exitCode === null) {
      const graceful = await Promise.race([exited.then(() => true), pause(2000).then(() => false)]);
      if (!graceful) { browser.kill(); await exited; }
    }
  }
}
