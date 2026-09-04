import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { collectRustSourceFiles } from "./ensure-runtime.mjs";

test("Runtime freshness excludes source files owned by independent binaries", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-runtime-sources-"));
  try {
    await fs.mkdir(path.join(root, "src", "bin"), { recursive: true });
    await fs.writeFile(path.join(root, "src", "main.rs"), "fn main() {}\n");
    await fs.writeFile(path.join(root, "src", "shared.rs"), "pub fn shared() {}\n");
    await fs.writeFile(path.join(root, "src", "bin", "docs.rs"), "fn main() {}\n");

    const files = await collectRustSourceFiles(path.join(root, "src"), {
      excludedTopLevelDirectories: new Set(["bin"]),
    });
    assert.deepEqual(
      files.map((file) => path.relative(root, file).replaceAll("\\", "/")),
      ["src/main.rs", "src/shared.rs"],
    );
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
