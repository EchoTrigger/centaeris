import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { fallbackFiles, writeThirdPartyLicenses } from "./third-party-licenses.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");

for (const binding of ["win32-x64-msvc", "linux-x64-gnu", "darwin-arm64"]) {
  test(`audited Rolldown license fallback covers ${binding}`, async () => {
    const item = { ecosystem: "npm", name: `@rolldown/binding-${binding}`, version: "1.1.5" };
    const files = await fallbackFiles(item, repoRoot, hostRoot);
    assert.equal(files.length, 1);
    assert.equal(files[0].source, "rolldown@1.1.5/LICENSE");
    assert.deepEqual(files[0].content, await fs.readFile(path.join(repoRoot, "node_modules/rolldown/LICENSE")));
    await assert.rejects(
      fallbackFiles({ ...item, version: "1.1.6" }, repoRoot, hostRoot),
      /third-party license files missing/,
    );
  });
}

test("third-party license assembly is complete and deterministic", async () => {
  const temporaryRoot = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-license-test-"));
  const first = path.join(temporaryRoot, "first");
  const second = path.join(temporaryRoot, "second");
  const tui = path.join(temporaryRoot, "tui");
  try {
    const firstCount = await writeThirdPartyLicenses(repoRoot, hostRoot, first);
    const secondCount = await writeThirdPartyLicenses(repoRoot, hostRoot, second);
    assert.equal(firstCount, secondCount);
    assert.ok(firstCount > 0);
    const firstIndex = await fs.readFile(path.join(first, "index.json"), "utf8");
    const secondIndex = await fs.readFile(path.join(second, "index.json"), "utf8");
    assert.equal(firstIndex, secondIndex);
    const index = JSON.parse(firstIndex);
    assert.ok(
      index.packages.every((item) => item.name !== "nono"),
      "the native Windows Runtime closure must not include the removed sandbox dependency",
    );
    for (const key of [
      "npm:@radix-ui/react-compose-refs@1.1.2",
      "rust:rmcp@3.1.4",
      "rust:tree-sitter@0.25.10",
    ]) {
      assert.ok(
        index.packages.some(
          (item) => `${item.ecosystem}:${item.name}@${item.version}` === key,
        ),
        `missing audited fallback ${key}`,
      );
    }
    assert.ok(
      index.packages.some((item) => item.ecosystem === "npm"
        && item.name.startsWith(`@rolldown/binding-${process.platform}-${process.arch}`)
        && item.version === "1.1.5"),
      "the license bundle must include the installed host's Rolldown binding",
    );
    await writeThirdPartyLicenses(
      repoRoot,
      hostRoot,
      tui,
      ["centaeris-runtime", "centaeris-tui"],
      false,
    );
    const tuiIndex = JSON.parse(await fs.readFile(path.join(tui, "index.json"), "utf8"));
    assert.ok(tuiIndex.packages.every((item) => item.ecosystem === "rust"));
    for (const key of ["rust:ratatui@0.29.0", "rust:rmcp@3.1.4"]) {
      assert.ok(
        tuiIndex.packages.some(
          (item) => `${item.ecosystem}:${item.name}@${item.version}` === key,
        ),
        `TUI package closure is missing ${key}`,
      );
    }
  } finally {
    await fs.rm(temporaryRoot, { recursive: true, force: true });
  }
});
