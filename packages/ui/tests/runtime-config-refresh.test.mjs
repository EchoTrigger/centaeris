import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

const readSource = (rootDir, relativePath) =>
  readFile(path.join(rootDir, relativePath), "utf8");

test("welcome pickers open down while conversation pickers open up", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const css = await readSource(rootDir, "src/styles/chat.css");

  assert.match(
    css,
    /\.composerPickerPanel,\s*\.contextWindowPanel\s*\{[\s\S]*?bottom: calc\(100% \+ 3px\)/,
  );
  assert.match(
    css,
    /\.chatBottomPlane\.is-welcome \.composerPickerPanel,\s*\.chatBottomPlane\.is-welcome \.contextWindowPanel\s*\{[\s\S]*?top: calc\(100% \+ 3px\);\s*bottom: auto/,
  );
  assert.match(css, /\.composerLucideIcon\.is-chevron\s*\{[\s\S]*?transform: rotate\(180deg\)/);
  assert.match(css, /\.chatBottomPlane\.is-welcome \.composerLucideIcon\.is-chevron\s*\{\s*transform: none/);
});

test("the runtime config bridge validates the shared notification payload", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const bridge = await readSource(rootDir, "src/lib/chatBridge.ts");

  assert.match(bridge, /listenHost<unknown>\("runtime\/config-changed"/);
  assert.match(bridge, /Object\.keys\(payload\)\.length !== 0/);
});
