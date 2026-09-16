import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { EventEmitter } from "node:events";
import test from "node:test";
import { connectRelayProcess } from "./wslRuntimeBridge.mjs";

const fixture = () => {
  const child = new EventEmitter();
  child.stdin = new PassThrough();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  child.kill = () => { child.killed = true; };
  return child;
};

test("relay consumes a split handshake and preserves subsequent UTF-8 frames", async () => {
  const child = fixture();
  const pending = connectRelayProcess(child);
  child.stdout.write('{"connected":');
  child.stdout.write('true}\n回答\n');
  const socket = await pending;
  socket.setEncoding("utf8");
  const data = new Promise((resolve) => socket.once("data", resolve));
  assert.equal(await data, "回答\n");
  const sent = new Promise((resolve) => child.stdin.once("data", resolve));
  socket.write("request\n");
  assert.equal(String(await sent), "request\n");
  socket.destroy();
  assert.equal(child.killed, true);
});

test("relay rejects malformed and oversized handshakes and cleans up", async () => {
  for (const header of ['{}\n', 'x'.repeat(1025)]) {
    const child = fixture();
    const pending = connectRelayProcess(child);
    child.stdout.write(header);
    await assert.rejects(pending, /handshake/);
    assert.equal(child.killed, true);
  }
});

test("relay fails promptly if the process exits before connecting", async () => {
  const child = fixture();
  const pending = connectRelayProcess(child);
  child.stderr.write("socket unavailable");
  child.emit("close", 1);
  await assert.rejects(pending, /socket unavailable/);
});
