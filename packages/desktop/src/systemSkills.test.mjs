import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { inspectSystemSkillsBundle } from "./systemSkills.mjs";

test("System Skill bundle inspector returns a stable content digest", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-system-skills-"));
  try {
    await fs.mkdir(path.join(root, "alpha"));
    await fs.writeFile(path.join(root, "alpha", "SKILL.md"), "---\nname: alpha\ndescription: alpha\n---\n");
    const first = await inspectSystemSkillsBundle(root);
    const second = await inspectSystemSkillsBundle(root);
    assert.deepEqual(first.skillNames, ["alpha"]);
    assert.match(first.digest, /^[0-9a-f]{64}$/);
    assert.equal(second.digest, first.digest);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
