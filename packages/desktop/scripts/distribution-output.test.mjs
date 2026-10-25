import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { reserveDistributionOutput } from "./distribution-output.mjs";

test("an external distribution build reserves a fresh directory without replacing existing files", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-output-test-"));
  try {
    const output = path.join(root, "application");
    assert.equal(await reserveDistributionOutput(output), output);
    await fs.writeFile(path.join(output, "keep.txt"), "existing application");
    await assert.rejects(reserveDistributionOutput(output), /already exists/);
    assert.equal(await fs.readFile(path.join(output, "keep.txt"), "utf8"), "existing application");
    await assert.rejects(reserveDistributionOutput("relative-output"), /absolute/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
