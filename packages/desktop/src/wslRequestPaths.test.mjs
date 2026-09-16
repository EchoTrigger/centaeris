import assert from "node:assert/strict";
import test from "node:test";
import { mapWslRequestPaths } from "./wslRequestPaths.mjs";
import { toLinuxPath } from "./wslPaths.mjs";
const map = (value, options) => toLinuxPath(value, "Ubuntu-24.04", options);

test("only declared attachment paths are translated; prompt text and IDs remain opaque", () => {
  const request = { message: "D:\\photo.png", operationId: "D:\\id", attachments: [{ placeholder: "image", localPath: "D:\\photo.png" }] };
  const result = mapWslRequestPaths("session/prompt", { request }, map);
  assert.equal(result.request.attachments[0].localPath, "/mnt/d/photo.png");
  assert.equal(result.request.message, request.message);
  assert.equal(result.request.operationId, request.operationId);
  assert.equal(request.attachments[0].localPath, "D:\\photo.png");
});

test("execution roots reject Windows mounts while plugin imports accept them", () => {
  assert.throws(() => mapWslRequestPaths("workspace_activate", { request: { root: "D:\\project" } }, map));
  assert.throws(() => mapWslRequestPaths("skill/source/add", { request: { path: "D:\\skills" } }, map));
  assert.equal(mapWslRequestPaths("plugin/install", { request: { sourcePath: "D:\\plugin" } }, map).request.sourcePath, "/mnt/d/plugin");
  const payload = { request: { path: "D:\\opaque" } };
  assert.equal(mapWslRequestPaths("unrelated", payload, map), payload);
});
