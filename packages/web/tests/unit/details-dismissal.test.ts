import assert from "node:assert/strict";
import { test } from "node:test";
import { dismissDetailsOnOutsideInteraction } from "../../src/components/detailsDismissal.ts";

test("context closes outside, stays open inside, and supports Escape and keyboard departure", () => {
  const listeners = new Map();
  let focused = false;
  const root = {
    open: true,
    contains: (node) => node === root,
    querySelector: () => ({ focus: () => { focused = true; } }),
    ownerDocument: {
      addEventListener: (name, listener) => listeners.set(name, listener),
      removeEventListener: (name) => listeners.delete(name),
    },
  };
  const cleanup = dismissDetailsOnOutsideInteraction(root as unknown as HTMLDetailsElement);
  listeners.get("pointerdown")({ target: root });
  assert.equal(root.open, true);
  listeners.get("pointerdown")({ target: {} });
  assert.equal(root.open, false);
  assert.equal(focused, false);
  root.open = true;
  listeners.get("keydown")({ key: "Escape", preventDefault() {} });
  assert.equal(root.open, false);
  assert.equal(focused, true);
  root.open = true;
  listeners.get("focusin")({ target: {} });
  assert.equal(root.open, false);
  cleanup();
  assert.equal(listeners.size, 0);
});
