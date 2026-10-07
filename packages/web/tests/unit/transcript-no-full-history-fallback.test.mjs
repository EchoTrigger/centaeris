import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

test("workspace routes do not call the legacy full-history projection", () => {
  const appRoute = source("../../src/routes/AppRoute.jsx");
  const trashRoute = source("../../src/routes/TrashSessionRoute.jsx");
  const streamTransport = source("../../src/chat/workspaceWebTransport.ts");

  for (const route of [appRoute, trashRoute]) {
    assert.doesNotMatch(route, /\/history(?:\?|["'`])/);
    assert.doesNotMatch(route, /\bvalidateHistoryPage\b/);
  }
  assert.doesNotMatch(appRoute, /chatViewStore|sessionEvents/);
  assert.doesNotMatch(streamTransport, /sessionEvents/);
});

test("workspace API no longer publishes the legacy session history page", () => {
  const workspaceApi = source("../../../api/app_core/http/workspaces.py");

  assert.doesNotMatch(workspaceApi, /def session_history\(/);
  assert.doesNotMatch(workspaceApi, /\/sessions\/\{session_id\}\/history/);
});
