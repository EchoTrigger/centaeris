import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("composer panels preserve their anchored layout", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const css = await readFile(path.join(rootDir, "src/styles/chat.css"), "utf8");

  assert.match(css, /\.slashCommandPanel \{[\s\S]*?bottom: calc\(100% - 1px\)/);
  assert.match(css, /\.input-wrapper:has\(\.slashCommandPanel\)/);
  assert.match(
    css,
    /\.composerPickerPanel,\s*\.contextWindowPanel\s*\{[\s\S]*?box-shadow: none/,
  );
  assert.match(css, /\.composerPickerPanel\.is-model \{[\s\S]*?width: min\(220px/);
  assert.match(css, /\.contextWindowPanel \{[\s\S]*?width: min\(420px/);
  assert.match(css, /\.mcpComposerPanel header \{\s*border-bottom: 0/);
});
