import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Local cross-client check; neither application depends on the other repository.
// Usage: node packages/ui/scripts/check-typography-parity.mjs <peer-client-src>
const peerSource = process.argv[2];
assert.ok(peerSource, "Pass the other client's src directory");
const localSource = fileURLToPath(new URL("../src/", import.meta.url));
const sources = [localSource, path.resolve(peerSource)];
for (const name of ["typography.css", "status-shimmer.css", "theme.css", "transcript-theme.css"]) {
  const styles = await Promise.all(sources.map((source) => readFile(path.join(source, "styles", name), "utf8")));
  assert.equal(styles[0].replaceAll("\r\n", "\n"), styles[1].replaceAll("\r\n", "\n"), `Client ${name} styles differ`);
}
for (const name of ["theme.ts", "useStreamPresentation.ts", "../public/theme-init.js"]) {
  const contents = await Promise.all(sources.map((source) => readFile(path.join(source, name), "utf8")));
  assert.equal(contents[0].replaceAll("\r\n", "\n"), contents[1].replaceAll("\r\n", "\n"), `Client ${name} logic differs`);
}
for (const name of ["WorkProgress.tsx", "workDuration.ts"]) {
  const contents = await Promise.all(sources.map((source, index) => readFile(path.join(source, index === 0 ? "components/chat" : "chat", name), "utf8")));
  const normalize = (text) => text.replaceAll("\r\n", "\n").replace('"../../i18n"', '"../i18n"');
  assert.equal(normalize(contents[0]), normalize(contents[1]), `Client ${name} logic differs`);
}
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
