import assert from "node:assert/strict";
import { test } from "node:test";
import {
  readPreferredModelIdentity,
  writePreferredModelIdentity,
  readModelThinkingMode,
  writeModelThinkingMode,
} from "../../src/preferences.js";

test("model and effort choices persist per user, workspace, and model", () => {
  const values = new Map();
  globalThis.window = {
    localStorage: {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    },
    dispatchEvent() {},
  };
  globalThis.Event = class Event {};
  const pro = {
    providerId: "google.default",
    modelName: "gemini-3.1-pro-preview",
    thinkingMode: "high",
    thinkingModes: ["low", "medium", "high"],
  };
  const flash = {
    ...pro,
    modelName: "gemini-3.8-flash",
    thinkingMode: "medium",
  };
  assert.equal(readModelThinkingMode("user-1", "workspace-1", pro), "high");
  writeModelThinkingMode("user-1", "workspace-1", pro, "low");
  writePreferredModelIdentity("user-1", "workspace-1", pro);
  assert.equal(readModelThinkingMode("user-1", "workspace-1", pro), "low");
  pro.thinkingMode = "medium";
  assert.equal(readModelThinkingMode("user-1", "workspace-1", pro), "low");
  assert.equal(readModelThinkingMode("user-1", "workspace-1", flash), "medium");
  assert.equal(readModelThinkingMode("user-2", "workspace-1", pro), "medium");
  assert.equal(readPreferredModelIdentity("user-1", "workspace-1"), JSON.stringify([pro.providerId, pro.modelName]));
  assert.equal(readPreferredModelIdentity("user-1", "workspace-2"), "");
  pro.thinkingModes = ["medium", "high"];
  assert.equal(readModelThinkingMode("user-1", "workspace-1", pro), "medium");
  delete globalThis.window;
  delete globalThis.Event;
});
