import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import {
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
