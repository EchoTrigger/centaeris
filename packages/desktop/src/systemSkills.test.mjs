import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { inspectSystemSkillsBundle } from "./systemSkills.mjs";

const bundledSkillsRoot = path.resolve(import.meta.dirname, "..", "..", "..", "system-skills");

test("public source includes the three built-in System Skills and upstream notices", async () => {
  const bundle = await inspectSystemSkillsBundle(bundledSkillsRoot);
  assert.deepEqual(bundle.skillNames, ["runtime-recovery", "skill-creator", "skill-installer"]);
  for (const name of ["skill-creator", "skill-installer"]) {
    const root = path.join(bundledSkillsRoot, name);
    const licenseName = name === "skill-creator" ? "license.txt" : "LICENSE.txt";
    const [license, notice] = await Promise.all([
      fs.readFile(path.join(root, licenseName), "utf8"),
      fs.readFile(path.join(root, "NOTICE"), "utf8"),
    ]);
    assert.match(license, /Apache License/);
    assert.match(notice, /openai\/skills/);
  }
});

test("System Skill bundle inspector returns a stable content digest", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-system-skills-"));
  try {
    await fs.mkdir(path.join(root, "alpha"));
    await fs.writeFile(path.join(root, "alpha", "SKILL.md"), "---\nname: alpha\ndescription: alpha\n---\n");
    const first = await inspectSystemSkillsBundle(root);
    await fs.mkdir(path.join(root, "alpha", "__pycache__"));
    await fs.writeFile(path.join(root, "alpha", "__pycache__", "helper.pyc"), "transient");
    const second = await inspectSystemSkillsBundle(root);
    assert.deepEqual(first.skillNames, ["alpha"]);
    assert.match(first.digest, /^[0-9a-f]{64}$/);
    assert.equal(second.digest, first.digest);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
