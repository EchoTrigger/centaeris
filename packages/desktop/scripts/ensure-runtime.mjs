import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");
const runtimeCrateRoot = path.join(repoRoot, "packages", "runtime");
const coreRoot = path.join(repoRoot, "packages", "core");

const args = process.argv.slice(2);
const checkOnly = args.includes("--check");
const profileIndex = args.indexOf("--profile");
const profileArg = profileIndex === -1 ? undefined : args[profileIndex + 1];
const profile = profileArg && profileArg !== "release" ? "debug" : "release";
const binaryName = `centaeris-runtime${process.platform === "win32" ? ".exe" : ""}`;
const binaryPath = path.join(repoRoot, "target", profile, binaryName);

const statMtimeMs = async (targetPath) => {
  try {
    return (await fs.stat(targetPath)).mtimeMs;
  } catch (error) {
    if (error?.code === "ENOENT") {
      return null;
    }
    throw error;
  }
};

const newerSourcesThan = async (binaryMtimeMs) => {
  const newer = [];
  const collectTree = async (rootDir) => {
    let entries;
    try {
      entries = await fs.readdir(rootDir, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") {
        return;
      }
      throw error;
    }
    for (const entry of entries) {
      const full = path.join(rootDir, entry.name);
      if (entry.isDirectory()) {
        await collectTree(full);
      } else if (entry.isFile() && entry.name.endsWith(".rs")) {
        const stat = await fs.stat(full);
        if (stat.mtimeMs > binaryMtimeMs) {
          newer.push(full);
        }
      }
    }
  };
  for (const manifest of [
    path.join(runtimeCrateRoot, "Cargo.toml"),
    path.join(coreRoot, "Cargo.toml"),
  ]) {
    const stat = await statMtimeMs(manifest);
    if (stat !== null && stat > binaryMtimeMs) {
      newer.push(manifest);
    }
  }
  for (const srcRoot of [
    path.join(runtimeCrateRoot, "src"),
    path.join(coreRoot, "src"),
  ]) {
    await collectTree(srcRoot);
  }
  return newer;
};

const runBuild = () => {
  const buildArgs = ["build", "--locked"];
  if (profile === "release") {
    buildArgs.push("--release");
  }
  buildArgs.push("-p", "centaeris-runtime");
  console.log(`[ensure-runtime] building ${profile} runtime...`);
  const result = spawnSync("cargo", buildArgs, {
    cwd: repoRoot,
    stdio: "inherit",
    shell: false,
  });
  if (result.error) {
    throw new Error(`cargo failed to start: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(
      `cargo build -p centaeris-runtime exited with code ${result.status}`,
    );
  }
};

const main = async () => {
  const binaryMtimeMs = await statMtimeMs(binaryPath);
  if (binaryMtimeMs === null) {
    if (checkOnly) {
      throw new Error(
        `[ensure-runtime] missing ${profile} runtime: ${binaryPath}`,
      );
    }
    runBuild();
    const after = await statMtimeMs(binaryPath);
    if (after === null) {
      throw new Error(
        `[ensure-runtime] build finished but runtime still missing: ${binaryPath}`,
      );
    }
    console.log("[ensure-runtime] ok");
    return;
  }
  const newer = await newerSourcesThan(binaryMtimeMs);
  if (newer.length === 0) {
    console.log(`[ensure-runtime] ${profile} runtime is fresh`);
    return;
  }
  if (checkOnly) {
    const sample = newer.slice(0, 5).join("\n");
    throw new Error(
      `[ensure-runtime] ${profile} runtime is older than ${newer.length} source file(s):\n${sample}`,
    );
  }
  runBuild();
  const rebuiltMtimeMs = await statMtimeMs(binaryPath);
  const stillNewer = await newerSourcesThan(rebuiltMtimeMs ?? 0);
  if (stillNewer.length > 0) {
    const sample = stillNewer.slice(0, 5).join("\n");
    throw new Error(
      `[ensure-runtime] ${profile} runtime still older than ${stillNewer.length} source file(s) after build:\n${sample}`,
    );
  }
  console.log("[ensure-runtime] ok");
};

await main();
