import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

const hostRoot = path.resolve(import.meta.dirname, "..");
const probeDirectory = path.join(hostRoot, "src", "syntax-check-probe");
const probePath = path.join(probeDirectory, "invalid-nested-module.mjs");

test("desktop check rejects a newly added nested JavaScript file with invalid syntax", () => {
  const packageManifest = JSON.parse(
    fs.readFileSync(path.join(hostRoot, "package.json"), "utf8"),
  );
  assert.equal(
    packageManifest.scripts["check:syntax"],
    "node ./scripts/check-javascript-syntax.mjs",
  );
  assert.match(packageManifest.scripts.check, /^npm run check:syntax &&/);

  fs.mkdirSync(probeDirectory, { recursive: true });
  fs.writeFileSync(probePath, "export const broken = ;\n", "utf8");

  try {
    const result = spawnSync(
      process.execPath,
      ["--run", "check:syntax"],
      {
        cwd: hostRoot,
        encoding: "utf8",
        env: process.env,
      },
    );

    assert.notEqual(
      result.status,
      0,
      "desktop check silently accepted a newly added JavaScript file with invalid syntax",
    );
    assert.match(
      `${result.stdout}\n${result.stderr}`,
      /invalid-nested-module\.mjs/,
      "desktop check failed for an unrelated reason instead of reporting the new file",
    );
  } finally {
    fs.rmSync(probeDirectory, { recursive: true, force: true });
  }
});
