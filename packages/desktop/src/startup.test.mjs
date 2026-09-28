import assert from "node:assert/strict";
import test from "node:test";
import { startDesktop } from "./startup.mjs";

test("the window becomes visible while Runtime initialization is still pending", async () => {
  let finish;
  let visible = false;
  const starting = startDesktop(() => new Promise(resolve => { finish = resolve; }), async () => { visible = true; });
  assert.equal(visible, true);
  finish();
  await starting;
});
