import assert from "node:assert/strict";
import { test } from "vitest";
import * as duration from "../src/components/chat/agentDuration.ts";

test("formats agent durations", () => {
  assert.equal(duration.formatHmsDuration(0), "0s");
  assert.equal(duration.formatHmsDuration(59_900), "59s");
  assert.equal(duration.formatHmsDuration(60_000), "1m 0s");
  assert.equal(duration.formatHmsDuration(3_599_000), "59m 59s");
  assert.equal(duration.formatHmsDuration(3_600_000), "1h 0m 0s");
  assert.equal(duration.formatHmsDuration(4_536_000), "1h 15m 36s");
  assert.equal(duration.formatProcessDuration(0), "1s");
});
