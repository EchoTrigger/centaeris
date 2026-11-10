import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { collectRustSourceFiles } from "./ensure-runtime.mjs";
import { runtimeArtifactPath } from "../src/main/runtimeArtifact.mjs";

test("Runtime artifacts use Cargo's default or explicit target directory", () => {
  const repository = path.resolve("repository");
  const sharedTarget = path.resolve("shared-target");
  assert.equal(runtimeArtifactPath(repository, "release", "win32", ""),
    path.join(repository, "target/release/centaeris-runtime.exe"));
  assert.equal(runtimeArtifactPath(repository, "debug", "win32", sharedTarget),
    path.join(sharedTarget, "debug/centaeris-runtime.exe"));
  assert.equal(runtimeArtifactPath(repository, "release", "linux", "../shared-target"),
    path.resolve(repository, "../shared-target/release/centaeris-runtime"));
});

test("Runtime freshness excludes source files owned by independent binaries", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-runtime-sources-"));
  try {
    for (const directory of ["bin", "examples", "tests"]) {
      await fs.mkdir(path.join(root, directory), { recursive: true });
      await fs.writeFile(path.join(root, directory, "entry.rs"), "fn main() {}\n");
    }
    await fs.writeFile(path.join(root, "main.rs"), "fn main() {}\n");
    await fs.writeFile(path.join(root, "shared.rs"), "pub fn shared() {}\n");

    const files = await collectRustSourceFiles(root, {
      excludedTopLevelDirectories: new Set(["bin", "examples", "tests"]),
    });
    assert.deepEqual(
      files.map((file) => path.relative(root, file).replaceAll("\\", "/")),
      ["main.rs", "shared.rs"],
    );
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
