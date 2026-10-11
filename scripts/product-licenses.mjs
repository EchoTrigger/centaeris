import fs from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const files = [
  ["LICENSE", "LICENSE.centaeris.txt"],
  ["LICENSE-APACHE", "LICENSE-APACHE"],
  ["LICENSING.md", "LICENSING.md"],
  ["COPYRIGHT", "COPYRIGHT"],
  ["packages/model-catalog/centaeris_model_catalog/logos/LICENSE",
    "packages/model-catalog/centaeris_model_catalog/logos/LICENSE"],
];

export async function writeProductLicenses(repoRoot, destinationRoot) {
  for (const [source, destination] of files) {
    const target = path.join(destinationRoot, destination);
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.copyFile(path.join(repoRoot, source), target);
  }
}

export async function checkProductLicenses(repoRoot, destinationRoot) {
  for (const [source, destination] of files) {
    const expected = await fs.readFile(path.join(repoRoot, source));
    const actual = await fs.readFile(path.join(destinationRoot, destination));
    if (!actual.equals(expected)) {
      throw new Error(`Product license content mismatch: ${destination}`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [command, destinationRoot] = process.argv.slice(2);
  if (!["write", "check"].includes(command) || !destinationRoot || process.argv.length !== 4) {
    throw new Error("Usage: node scripts/product-licenses.mjs <write|check> <distribution-root>");
  }
  const repoRoot = path.resolve(import.meta.dirname, "..");
  await (command === "write" ? writeProductLicenses : checkProductLicenses)(repoRoot, path.resolve(destinationRoot));
}
