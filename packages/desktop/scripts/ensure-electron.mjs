import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const hostRoot = path.resolve(import.meta.dirname, "..");
const electronRoot = path.join(hostRoot, "node_modules", "electron");
const executable = path.join(electronRoot, "dist", "electron.exe");
if (!fs.existsSync(executable)) {
  const env = { ...process.env };
  delete env.npm_lifecycle_event;
  const result = spawnSync(process.execPath, [path.join(electronRoot, "install.js")], {
    cwd: hostRoot,
    env,
    stdio: "inherit",
    shell: false,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Electron install exited with code ${result.status}`);
}
console.log(`Electron runtime ready: ${executable}`);
