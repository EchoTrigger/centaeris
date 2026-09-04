import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

const filesBelow = async (root, relative = "") => {
  const entries = await fs.readdir(path.join(root, relative), { withFileTypes: true });
  const files = [];
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const next = path.join(relative, entry.name);
    if (entry.isSymbolicLink()) {
      throw new Error(`System Skill bundle must not contain symbolic links: ${next}`);
    }
    if (entry.isDirectory()) {
      files.push(...await filesBelow(root, next));
    } else if (entry.isFile()) {
      files.push(next);
    } else {
      throw new Error(`System Skill bundle contains an unsupported entry: ${next}`);
    }
  }
  return files;
};

export const inspectSystemSkillsBundle = async (root) => {
  const entries = await fs.readdir(root, { withFileTypes: true });
  const skillNames = [];
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) {
      throw new Error(`System Skill bundle root may only contain Skill directories: ${entry.name}`);
    }
    const manifest = path.join(root, entry.name, "SKILL.md");
    if (!(await fs.stat(manifest).then((stat) => stat.isFile(), () => false))) {
      throw new Error(`System Skill is missing SKILL.md: ${entry.name}`);
    }
    skillNames.push(entry.name);
  }
  if (skillNames.length === 0) {
    throw new Error("System Skill bundle must contain at least one Skill");
  }
  const hash = createHash("sha256");
  for (const relative of await filesBelow(root)) {
    hash.update(relative.replaceAll("\\", "/"));
    hash.update("\0");
    hash.update(await fs.readFile(path.join(root, relative)));
    hash.update("\0");
  }
  return { digest: hash.digest("hex"), skillNames };
};
