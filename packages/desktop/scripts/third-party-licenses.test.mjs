import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { writeThirdPartyLicenses } from "./third-party-licenses.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");

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
    for (const key of [
      "npm:@radix-ui/react-compose-refs@1.1.2",
      "npm:@rolldown/binding-win32-x64-msvc@1.1.5",
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
