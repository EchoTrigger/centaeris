import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const script = path.resolve(import.meta.dirname, "../installer/preflight.ps1");
const powershell = path.join(process.env.SystemRoot ?? "C:/Windows", "System32/WindowsPowerShell/v1.0/powershell.exe");
const windowsTest = process.platform === "win32" ? test : test.skip;

windowsTest("installer leaves a busy installation and its Runtime intact", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-installer-test-"));
  const installed = path.join(root, "项目's Desktop");
  const sibling = path.join(root, "项目's Desktop-other");
  await fs.mkdir(installed);
  await fs.mkdir(sibling);
  await fs.writeFile(path.join(sibling, "Centaeris Desktop.exe"), "installed application");
  const executable = path.join(sibling, "centaeris-runtime.exe");
  await fs.copyFile(process.execPath, executable);
  const child = spawn(executable, ["-e", "setInterval(() => {}, 1000)"], { windowsHide: true, stdio: "ignore" });
  await new Promise((resolve, reject) => { child.once("spawn", resolve); child.once("error", reject); });
  const probe = directory => spawnSync(powershell, [
    "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script,
    "-InstallDirectory", directory, "-WaitSeconds", "0",
  ], { encoding: "utf8", timeout: 15_000, windowsHide: true });
  try {
    const unrelated = probe(installed);
    assert.equal(unrelated.status, 0, unrelated.stdout + unrelated.stderr);
    const busy = probe(sibling);
    assert.equal(busy.status, 2, busy.stdout + busy.stderr);
    assert.match(busy.stdout, /still in use/i);
    assert.equal(child.exitCode, null, "installation must not terminate an active Runtime");
    assert.equal(await fs.stat(executable).then(() => true), true);
  } finally {
    child.kill();
    await new Promise(resolve => child.exitCode !== null ? resolve() : child.once("exit", resolve));
    await fs.rm(root, { recursive: true, force: true });
  }
});

windowsTest("installer accepts an unused directory and rejects a filesystem root", () => {
  const probe = directory => spawnSync(powershell, [
    "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script,
    "-InstallDirectory", directory, "-WaitSeconds", "0",
  ], { encoding: "utf8", timeout: 15_000, windowsHide: true });
  const unused = probe(path.join(os.tmpdir(), "centaeris-unused-install-target"));
  assert.equal(unused.status, 0, unused.stdout + unused.stderr);
  assert.notEqual(probe(path.parse(process.cwd()).root).status, 0);
});


windowsTest("installer preserves unrelated files and refuses a configured user data directory", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-installer-preserve-"));
  const document = path.join(root, "project.txt");
  await fs.writeFile(document, "keep my project");
  const probe = (directory, env = process.env) => spawnSync(powershell, [
    "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script,
    "-InstallDirectory", directory, "-WaitSeconds", "0",
  ], { env, encoding: "utf8", timeout: 15_000, windowsHide: true });
  try {
    assert.notEqual(probe(root).status, 0, "setup must refuse an existing workspace directory");
    const data = path.join(root, "custom-data");
    await fs.mkdir(data);
    assert.notEqual(probe(data, { ...process.env, CENTAERIS_DESKTOP_DATA_DIR: data }).status, 0);
    assert.equal(await fs.readFile(document, "utf8"), "keep my project");
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
