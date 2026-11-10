import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Stream cadence is host-specific and protected by each client's stream tests.
// Local cross-client check; neither application depends on the other repository.
// Usage: node packages/desktop/scripts/check-typography-parity.mjs <peer-client-src>
const peerSource = process.argv[2];
assert.ok(peerSource, "Pass the other client's src directory");
const localSource = fileURLToPath(new URL("../src/renderer", import.meta.url));
const sources = [localSource, path.resolve(peerSource)];
for (const name of ["typography.css", "status-shimmer.css", "chat-typography.css"]) {
  const styles = await Promise.all(sources.map((source) => readFile(path.join(source, "styles", name), "utf8")));
  assert.equal(styles[0].replaceAll("\r\n", "\n"), styles[1].replaceAll("\r\n", "\n"), `Client ${name} styles differ`);
}
const sourceRequires = sources.map((source) => createRequire(path.join(source, "theme.ts")));
for (const name of ["theme.css", "theme-store"]) {
  const contents = await Promise.all(sourceRequires.map((require) => readFile(require.resolve(`@centaeris/ui/${name}`), "utf8")));
  assert.equal(contents[0].replaceAll("\r\n", "\n"), contents[1].replaceAll("\r\n", "\n"), `Shared ${name} definitions differ`);
}
const themeInitializers = await Promise.all([
  fileURLToPath(new URL("../public/theme-init.js", import.meta.url)),
  path.resolve(peerSource, "../public/theme-init.js"),
].map((file) => readFile(file, "utf8")));
assert.equal(themeInitializers[0].replaceAll("\r\n", "\n"), themeInitializers[1].replaceAll("\r\n", "\n"), "Client theme-init.js logic differs");
// Conversation layout and disclosure behavior are host-specific.
const inventories = await Promise.all(sources.map(async (source) => {
  const directory = path.join(source, "assets/fonts");
  const names = (await readdir(directory)).sort();
  return Promise.all(names.map(async (name) => {
    const content = await readFile(path.join(directory, name));
    const bytes = name.endsWith(".css") ? content.toString("utf8").replaceAll("\r\n", "\n") : content;
    return [name, createHash("sha256").update(bytes).digest("hex")];
  }));
}));
assert.deepEqual(inventories[0], inventories[1], "Bundled font files differ");
console.log(`Typography tokens and ${inventories[0].length} bundled font files match.`);
