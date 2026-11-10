import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { ensureDefaultWorkspaceDirectory } from "./defaultWorkspace.mjs";

test("default workspace reuses the local daily directory", async () => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-default-workspace-"));
  try {
    const now = new Date(2026, 7, 2, 23, 59, 59);
    const expected = path.join(home, "centaeris-cwd-20260802");
    assert.equal(await ensureDefaultWorkspaceDirectory(home, now), expected);
    assert.equal(await ensureDefaultWorkspaceDirectory(home, now), expected);
    assert.equal((await fs.stat(expected)).isDirectory(), true);
  } finally {
    await fs.rm(home, { recursive: true, force: true });
  }
});
