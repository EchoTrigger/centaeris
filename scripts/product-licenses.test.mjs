import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { writeProductLicenses, checkProductLicenses } from "./product-licenses.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const logoLicense = "packages/model-catalog/centaeris_model_catalog/logos/LICENSE";

test("product distributions retain AGPL, Apache, scope, ownership and logo notices", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-product-licenses-"));
  try {
    await writeProductLicenses(repoRoot, root);
    for (const [source, destination] of [
      ["LICENSE", "LICENSE.centaeris.txt"],
      ["LICENSE-APACHE", "LICENSE-APACHE"],
      ["LICENSING.md", "LICENSING.md"],
      ["COPYRIGHT", "COPYRIGHT"],
      [logoLicense, logoLicense],
    ]) {
      assert.deepEqual(await fs.readFile(path.join(root, destination)),
        await fs.readFile(path.join(repoRoot, source)));
    }
    await checkProductLicenses(repoRoot, root);
    await fs.writeFile(path.join(root, "LICENSE-APACHE"), "truncated license");
    await assert.rejects(checkProductLicenses(repoRoot, root), /LICENSE-APACHE/);
    await writeProductLicenses(repoRoot, root);
    await fs.unlink(path.join(root, logoLicense));
    await assert.rejects(checkProductLicenses(repoRoot, root), /ENOENT/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
