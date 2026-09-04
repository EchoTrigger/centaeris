import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

const readSource = (rootDir, relativePath) =>
  readFile(path.join(rootDir, relativePath), "utf8");

test("composer uses one mutually exclusive panel state", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const source = await readSource(
    rootDir,
    "src/components/chat/ChatComposer.tsx",
  );

  assert.match(
    source,
    /type ComposerPanel = "commands" \| "model" \| "reasoning" \| "context" \| "mcp" \| "mcp-configure" \| null/,
  );
  assert.match(source, /const \[activePanel, setActivePanel\] = useState<ComposerPanel>\(null\)/);
  assert.match(source, /current === panel \? null : panel/);
  assert.match(source, /closeOnOutsidePointer/);
  assert.doesNotMatch(source, /<details className="composerPicker/);
});

test("slash commands reuse existing actions and replace the compact chip", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const source = await readSource(
    rootDir,
    "src/components/chat/ChatComposer.tsx",
  );
  const css = await readSource(rootDir, "src/styles/chat.css");

  for (const command of [
    "/new",
    "/model",
    "/effort",
    "/state",
    "/compact",
    "/models",
    "/skills",
    "/plugins",
    "/mcp",
  ]) {
    assert.match(source, new RegExp(`name: "${command.replace("/", "\\/")}"`));
  }
  assert.match(source, /case "compact":[\s\S]*?onCompact\(\)/);
  assert.doesNotMatch(source, /compact-chip/);
  assert.match(css, /\.slashCommandPanel \{[\s\S]*?bottom: calc\(100% - 1px\)/);
  assert.match(css, /\.input-wrapper:has\(\.slashCommandPanel\)/);
  assert.match(
    css,
    /\.composerPickerPanel,\s*\.contextWindowPanel\s*\{[\s\S]*?box-shadow: none/,
  );
  assert.match(css, /\.composerPickerPanel\.is-model \{[\s\S]*?width: min\(220px/);
  assert.match(css, /\.contextWindowPanel \{[\s\S]*?width: min\(420px/);
  assert.match(source, /formatContextTokenCount/);
  assert.match(source, /\{tool\.providerId\} · \{tool\.name\}/);
  assert.doesNotMatch(
    source,
    /The breakdown appears when the next model request starts\./,
  );
  assert.match(source, /className="slashCommandPanel mcpComposerPanel"/);
  assert.match(source, /Save & test/);
  assert.match(source, /Saved · applies to next run/);
  assert.match(css, /\.mcpComposerPanel header \{\s*border-bottom: 0/);
  assert.doesNotMatch(source, /catalog snapshot locked/);
});
