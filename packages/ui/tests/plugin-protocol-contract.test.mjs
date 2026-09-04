import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "vitest";

const dialogUrl = new URL("../src/components/PluginsDialog.tsx", import.meta.url);
const bridgeUrl = new URL("../src/lib/chatBridge.ts", import.meta.url);

test("Plugins uses the single-purpose plugin protocol", async () => {
  const [dialogSource, bridgeSource] = await Promise.all([
    readFile(dialogUrl, "utf8"),
    readFile(bridgeUrl, "utf8"),
  ]);

  assert.match(dialogSource, /listPlugins\(\)/);
  assert.match(dialogSource, /getPluginDetail\(\{ id: item\.id \}\)/);
  assert.match(dialogSource, /sequence !== detailSequence\.current/);
  assert.match(dialogSource, /setPluginEnabled\(\{ id: item\.id, enabled:/);
  assert.match(dialogSource, /Select a plugin/);
  assert.match(dialogSource, /Reload plugins/);
  assert.doesNotMatch(dialogSource, /pluginsHeader|iconText|sourcePath/);
  assert.match(bridgeSource, /"plugin\/list"/);
  assert.match(bridgeSource, /"plugin\/detail"/);
  assert.match(bridgeSource, /"plugin\/set_enabled"/);
  assert.match(bridgeSource, /"plugin\/source_ref"/);
  assert.doesNotMatch(bridgeSource, /type PluginKind/);
});
