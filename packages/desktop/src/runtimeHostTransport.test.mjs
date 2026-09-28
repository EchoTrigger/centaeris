import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import {
  waitForRuntimeSocket,
  createRuntimeResponseTracker,
  requireRuntimeDescriptor,
  stopStaleRuntimeSocket,
} from "./runtimeHostTransport.mjs";

const buildId = `sha256:${"a".repeat(64)}`;
const descriptor = {
  status: "ok",
  runtime: "centaeris-runtime",
  protocol: "centaeris.runtime",
  protocolVersion: 1,
  capabilities: ["json_rpc_2_over_jsonl"],
  events: ["session/update", "runtime/config-changed"],
  projections: ["runtime_event", "session_event", "headless_transcript"],
  buildId,
  coreProtocolVersion: "1.0.0",
  profileId: "profile",
  storeId: "store",
  storeSchemaVersion: 1,
  layoutSchemaVersion: 1,
};

test("Runtime initialize descriptor rejects unknown v1 fields", () => {
  assert.equal(requireRuntimeDescriptor(descriptor, buildId), descriptor);
  assert.throws(
    () => requireRuntimeDescriptor({ ...descriptor, extra: true }, buildId),
    (error) => error.code === "runtime_descriptor_mismatch" && /unknown fields: extra/.test(error.message),
  );
  const legacyDescriptor = Object.fromEntries(
    Object.entries(descriptor).filter(([field]) => field !== "buildId"),
  );
  assert.throws(
    () => requireRuntimeDescriptor(legacyDescriptor, buildId),
    (error) => error.code === "runtime_descriptor_mismatch",
  );
  for (const [field, item] of [
    ["capabilities", "json_rpc_2_over_jsonl"],
    ["projections", "headless_transcript"],
  ]) {
    assert.throws(
      () => requireRuntimeDescriptor({
        ...descriptor,
        [field]: descriptor[field].filter((value) => value !== item),
      }, buildId),
      (error) => error.code === "runtime_descriptor_mismatch"
        && error.message.includes(`${field} is missing ${item}`),
    );
  }
});

test("stale Runtime replacement continues when app_exit never responds", async () => {
  const socket = new EventEmitter();
  socket.destroyed = false;
  socket.end = () => assert.fail("an unresponsive Runtime must not receive a graceful socket end");
  socket.destroy = () => {
    socket.destroyed = true;
    socket.emit("close");
  };

  await stopStaleRuntimeSocket({
    socket,
    requestAppExit: () => new Promise(() => {}),
    appExitTimeoutMs: 5,
    staleServerShutdownMs: 0,
  });

  assert.equal(socket.destroyed, true);
});

test("Runtime request timeout reports an unknown outcome and consumes one late response", async () => {
  const tracker = createRuntimeResponseTracker({ requestTimeoutMs: 5 });
  const response = tracker.track("electron-1", {
    command: "session/new",
    group: "session",
  });

  await assert.rejects(
    response,
    (error) => error.code === "runtime_request_timeout"
      && error.outcomeUnknown === true
      && error.command === "session/new",
  );
  assert.deepEqual(tracker.take("electron-1"), { state: "abandoned" });
  assert.deepEqual(tracker.take("electron-1"), { state: "unknown" });
  assert.deepEqual(tracker.take("never-issued"), { state: "unknown" });
});


test("Runtime startup waits for profile recovery beyond five seconds", async () => {
  let elapsed = 0;
  const socket = {};
  const result = await waitForRuntimeSocket({
    connect: async () => { if (elapsed < 6500) throw new Error("connect ENOENT"); return socket; },
    getStartFailure: () => null,
    now: () => elapsed,
    wait: async (ms) => { elapsed += ms; },
  });
  assert.equal(result, socket);
  assert.equal(elapsed, 6500);
});

test("Runtime startup fails promptly with the server error and has a bounded deadline", async () => {
  let elapsed = 0;
  const failure = new Error("profile recovery failed");
  await assert.rejects(waitForRuntimeSocket({
    connect: async () => { throw new Error("connect ENOENT"); },
    getStartFailure: () => elapsed >= 200 ? failure : null,
    now: () => elapsed,
    wait: async (ms) => { elapsed += ms; },
  }), (error) => error === failure);
  assert.equal(elapsed, 200);
  elapsed = 0;
  await assert.rejects(waitForRuntimeSocket({
    connect: async () => { throw new Error("connect ENOENT"); },
    getStartFailure: () => null,
    now: () => elapsed,
    wait: async (ms) => { elapsed += ms; },
  }), /Runtime Server startup timed out after 30000 ms.*ENOENT/);
  assert.equal(elapsed, 30000);
});
