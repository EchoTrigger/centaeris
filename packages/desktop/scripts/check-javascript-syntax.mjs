import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const hostRoot = path.resolve(import.meta.dirname, "..");
const sourceRoots = ["scripts", "src"];
const javaScriptExtensions = new Set([".cjs", ".js", ".mjs"]);

function collectJavaScriptFiles(directory) {
  const files = [];
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const entryPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...collectJavaScriptFiles(entryPath));
    } else if (entry.isFile() && javaScriptExtensions.has(path.extname(entry.name))) {
      files.push(entryPath);
    }
  }
  return files;
}

const javaScriptFiles = sourceRoots
  .flatMap((sourceRoot) => collectJavaScriptFiles(path.join(hostRoot, sourceRoot)))
  .sort((left, right) => left.localeCompare(right, "en"));

for (const filePath of javaScriptFiles) {
  const result = spawnSync(process.execPath, ["--check", filePath], {
    cwd: hostRoot,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    process.exitCode = result.status ?? 1;
    break;
  }
}
