import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";
import { collectThirdPartyLicenses } from "./third-party-licenses.mjs";
import { inspectSystemSkillsBundle } from "../src/systemSkills.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");
const sha256 = (content) => createHash("sha256").update(content).digest("hex");

const args = process.argv.slice(2);
const valueAfter = (name) => {
  const index = args.indexOf(name);
  return index === -1 ? undefined : args[index + 1];
};
const distRoot = valueAfter("--dist");
const expectedSystemSkillsRoot = valueAfter("--system-skills-source");
const licenseRoot = valueAfter("--licenses-only") ??
  (distRoot ? path.join(path.resolve(distRoot), "THIRD_PARTY_LICENSES") : undefined);
if (!licenseRoot) throw new Error("use --dist <app-root> or --licenses-only <license-root>");

const index = JSON.parse(await fs.readFile(path.join(licenseRoot, "index.json"), "utf8"));
if (index.schema !== "centaeris_third_party_licenses_v1") {
  throw new Error("third-party license index schema mismatch");
}
const expected = await collectThirdPartyLicenses(repoRoot, hostRoot);
const expectedKeys = expected.map((item) => `${item.ecosystem}:${item.name}@${item.version}`).sort();
const actualKeys = index.packages
  .map((item) => `${item.ecosystem}:${item.name}@${item.version}`)
  .sort();
if (JSON.stringify(actualKeys) !== JSON.stringify(expectedKeys)) {
  throw new Error("third-party license package closure mismatch");
}
for (const item of index.packages) {
  if (!item.license || !item.source || item.files.length === 0) {
    throw new Error(`third-party license entry incomplete: ${item.name}@${item.version}`);
  }
  for (const file of item.files) {
    const content = await fs.readFile(path.join(licenseRoot, file.path));
    if (sha256(content) !== file.sha256) {
      throw new Error(`third-party license digest mismatch: ${file.path}`);
    }
  }
}

if (distRoot) {
  const required = [
    "LICENSE",
    "LICENSES.chromium.html",
    "LICENSE.centaeris.txt",
    "COPYRIGHT",
    "THIRD_PARTY_NOTICES.md",
    "resources/ui-dist/licenses/OFL-GoogleSansCode.txt",
    "resources/ui-dist/licenses/OFL-NotoSansCJK.txt",
  ];
  for (const relativePath of required) {
    await fs.access(path.join(distRoot, relativePath));
  }
  const notices = await fs.readFile(path.join(distRoot, "THIRD_PARTY_NOTICES.md"), "utf8");
  for (const relativePath of required.slice(-2)) {
    if (!notices.includes(relativePath)) {
      throw new Error(`dist notice does not link bundled font license: ${relativePath}`);
    }
  }
  const systemSkillsRoot = path.join(distRoot, "resources", "system-skills");
  let systemSkillEntries = null;
  try {
    systemSkillEntries = await fs.readdir(systemSkillsRoot, { withFileTypes: true });
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (systemSkillEntries) {
    const packagedBundle = await inspectSystemSkillsBundle(systemSkillsRoot);
    if (expectedSystemSkillsRoot) {
      const expectedBundle = await inspectSystemSkillsBundle(expectedSystemSkillsRoot);
      if (packagedBundle.digest !== expectedBundle.digest) {
        throw new Error("packaged System Skill bundle does not match the expected source");
      }
    }
    for (const entry of systemSkillEntries) {
      if (!entry.isDirectory()) continue;
      const skillRoot = path.join(systemSkillsRoot, entry.name);
      const files = await fs.readdir(skillRoot);
      await fs.access(path.join(skillRoot, "SKILL.md"));
      if (files.includes("NOTICE") && !files.some((name) => /^license(?:\..+)?$/i.test(name))) {
        throw new Error(`bundled System Skill NOTICE has no matching license file: ${entry.name}`);
      }
    }
  } else if (expectedSystemSkillsRoot) {
    throw new Error("expected System Skill bundle is missing from the desktop dist");
  }
}

console.log(`Third-party license check passed: ${actualKeys.length} packages`);
