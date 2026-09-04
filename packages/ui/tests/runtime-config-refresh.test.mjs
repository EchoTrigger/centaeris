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

test("desktop refreshes shared runtime config from the Runtime notification", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const [app, bridge] = await Promise.all([
    readSource(rootDir, "src/App.tsx"),
    readSource(rootDir, "src/lib/chatBridge.ts"),
  ]);

  assert.match(bridge, /listenHost<unknown>\("runtime\/config-changed"/);
  assert.match(bridge, /Object\.keys\(payload\)\.length !== 0/);
  assert.match(app, /listenAgentRuntimeConfigChanges\(\(\) => \{/);
  assert.match(app, /setRuntimeConfigRevision\(\(revision\) => revision \+ 1\)/);
  assert.match(app, /getAgentRuntimeConfig\(\)\.then\(\(config\) => \{/);
});
