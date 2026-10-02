import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { assertCleanExit, waitForExit, withCleanup } from "./smoke-runtime-cleanup.mjs";

class ControlledChild extends EventEmitter {
  exitCode = null;
  signalCode = null;
  kills = 0;
  exit(code, signal = null) {
    this.exitCode = code;
    this.signalCode = signal;
    this.emit("exit", code, signal);
  }
  kill() {
    this.kills += 1;
    queueMicrotask(() => this.exit(null, "SIGTERM"));
  }
}

test("cleanup accepts natural exit after the old seven-second sample without killing", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const child = new ControlledChild();
  const result = assertCleanExit(child, "reconnected Runtime");
  const verified = assert.doesNotReject(result);
  setTimeout(() => child.exit(0), 7_036);
  t.mock.timers.tick(7_000);
  await Promise.resolve();
  await Promise.resolve();
  t.mock.timers.tick(36);
  await verified;
  assert.equal(child.kills, 0);
});

test("cleanup fails and kills a child that never exits at the existing bounded deadline", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const child = new ControlledChild();
  const result = assertCleanExit(child, "replacement Runtime");
  const verified = assert.rejects(result, /did not exit within 30000ms/);
  t.mock.timers.tick(29_999);
  await Promise.resolve();
  const killsBeforeDeadline = child.kills;
  t.mock.timers.tick(1);
  await verified;
  assert.equal(killsBeforeDeadline, 0);
  assert.equal(child.kills, 1);
});

for (const [code, signal] of [[7, null], [null, "SIGTERM"]]) {
  test(`cleanup rejects actual exit code=${code} signal=${signal}`, async (t) => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const child = new ControlledChild();
    const result = assertCleanExit(child, "Runtime");
    const verified = assert.rejects(result, new RegExp(`code=${code} signal=${signal}`));
    child.exit(code, signal);
    t.mock.timers.tick(7_000);
    await verified;
    assert.equal(child.kills, 0);
  });
}

test("waitForExit preserves already-exited status without a timer or kill", async () => {
  for (const [code, signal] of [[0, null], [9, null], [null, "SIGTERM"]]) {
    const child = new ControlledChild();
    child.exit(code, signal);
    assert.deepEqual(await waitForExit(child), { code, signal });
    assert.equal(child.kills, 0);
  }
});

test("cleanup accepts an already cleanly exited child and absent owned child", async () => {
  const child = new ControlledChild();
  child.exit(0);
  await assertCleanExit(child, "Runtime");
  await assertCleanExit(null, "Runtime");
  assert.equal(child.kills, 0);
});

test("cleanup retains the original failure and EPERM and continues owned-child cleanup", async () => {
  const original = new Error("transport failed");
  const cleanupError = Object.assign(new Error("unlink stale-runtime.exe"), { code: "EPERM" });
  const order = [];
  await assert.rejects(withCleanup(
    async () => { throw original; },
    async () => { order.push("shutdown"); throw new Error("shutdown failed"); },
    async () => { order.push("owned-child"); },
    async () => { order.push("remove"); throw cleanupError; },
  ), (error) => {
    assert.ok(error instanceof AggregateError);
    assert.equal(error.errors[0], original);
    assert.equal(error.errors[2], cleanupError);
    assert.equal(error.cause, original);
    return true;
  });
  assert.deepEqual(order, ["shutdown", "owned-child", "remove"]);
});

test("EPERM after a successful operation still fails", async () => {
  const error = Object.assign(new Error("unlink failed"), { code: "EPERM" });
  await assert.rejects(withCleanup(async () => "ok", async () => { throw error; }),
    (actual) => actual === error);
});

test("cleanup preserves a lone operation error and successful return value", async () => {
  const error = new Error("assertion failed");
  await assert.rejects(withCleanup(async () => { throw error; }, async () => {}),
    (actual) => actual === error);
  assert.equal(await withCleanup(async () => 42, async () => {}), 42);
});
