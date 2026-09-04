#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const tuiRoot = realpathSync(dirname(fileURLToPath(import.meta.url)));
const repoRoot = resolve(tuiRoot, "../..");
const runtimeName = process.platform === "win32"
  ? "centaeris-runtime.exe"
  : "centaeris-runtime";
const runtimePath = process.env.CENTAERIS_RUNTIME_EXE ?? resolve(
  repoRoot,
  "target/release",
  runtimeName,
);

if (!existsSync(runtimePath)) {
  console.error(`Centa release Runtime not found: ${runtimePath}`);
  console.error("Run .\\scripts\\build-desktop.ps1 from the Centaeris repository first.");
  process.exit(1);
}

const result = spawnSync(
  process.platform === "win32" ? "cargo.exe" : "cargo",
  [
    "run",
    "--quiet",
    "--locked",
    "--manifest-path",
    resolve(tuiRoot, "Cargo.toml"),
    "--",
    ...process.argv.slice(2),
  ],
  {
    cwd: process.cwd(),
    env: { ...process.env, CENTAERIS_RUNTIME_EXE: runtimePath },
    stdio: "inherit",
  },
);

if (result.error) {
  console.error(`Failed to start Centa TUI: ${result.error.message}`);
  process.exit(1);
}
if (result.status === null) {
  console.error(`Centa TUI terminated by signal: ${result.signal ?? "unknown"}`);
  process.exit(1);
}
process.exit(result.status);
