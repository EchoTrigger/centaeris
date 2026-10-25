import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { validateInstallerInput } from "./installer-input.mjs";

test("installer requires a complete app payload and matching application version", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-installer-input-"));
  const files = ["Centaeris Desktop.exe", "resources/bin/centaeris-runtime.exe",
    "resources/ui-dist/index.html", "resources/system-skills/example/SKILL.md",
    "LICENSE.centaeris.txt", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES/index.json"];
  try {
    await assert.rejects(validateInstallerInput(root, "0.1.0"));
    for (const file of files) {
      await fs.mkdir(path.dirname(path.join(root, file)), { recursive: true });
      await fs.writeFile(path.join(root, file), "fixture");
    }
    await fs.mkdir(path.join(root, "resources/app"), { recursive: true });
    const manifest = path.join(root, "resources/app/package.json");
    await fs.writeFile(manifest, JSON.stringify({ name: "@centaeris/electron-host", version: "0.1.0", main: "src/main.mjs" }));
    await validateInstallerInput(root, "0.1.0");
    await fs.writeFile(manifest, JSON.stringify({ name: "@centaeris/electron-host", version: "0.0.9", main: "src/main.mjs" }));
    await assert.rejects(validateInstallerInput(root, "0.1.0"), /version mismatch/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
