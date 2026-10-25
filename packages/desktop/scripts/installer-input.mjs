import fs from "node:fs/promises";
import path from "node:path";
import { inspectSystemSkillsBundle } from "../src/systemSkills.mjs";

export async function validateInstallerInput(directory, expectedVersion) {
  const required = ["Centaeris Desktop.exe", "resources/bin/centaeris-runtime.exe",
    "resources/ui-dist/index.html", "LICENSE.centaeris.txt",
    "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES/index.json"];
  for (const relative of required) {
    if (!(await fs.stat(path.join(directory, relative))).isFile()) {
      throw new Error("Installer payload requires a file: " + relative);
    }
  }
  await inspectSystemSkillsBundle(path.join(directory, "resources/system-skills"));
  const manifest = JSON.parse(await fs.readFile(path.join(directory, "resources/app/package.json"), "utf8"));
  if (manifest.name !== "@centaeris/electron-host" || manifest.main !== "src/main.mjs") {
    throw new Error("Installer payload is not a Centaeris Desktop application");
  }
  if (manifest.version !== expectedVersion) throw new Error("Installer and packaged application version mismatch");
}
