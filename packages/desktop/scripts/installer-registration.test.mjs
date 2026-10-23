import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const script = path.resolve(import.meta.dirname, "installer-registration.ps1");
const powershell = path.join(process.env.SystemRoot ?? "C:/Windows", "System32/WindowsPowerShell/v1.0/powershell.exe");
const windowsTest = process.platform === "win32" ? test : test.skip;
const run = (file, args = []) => spawnSync(powershell, [
  "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", file, ...args,
], { encoding: "utf8", timeout: 15_000, windowsHide: true });
const query = registryRoot => run(script, ["-RegistryRoot", registryRoot]);

windowsTest("installer registration treats a missing uninstall root as a clean user profile", () => {
  const result = query(`HKCU:\\Software\\CentaerisInstallerTest-${randomUUID()}\\Uninstall`);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.equal(result.stdout.trim(), "");
  assert.equal(result.stderr.trim(), "");
});

windowsTest("installer registration preserves installed identity and ignores unrelated applications", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-registration-test-"));
  const fixture = path.join(directory, "fixture.ps1");
  const registryRoot = `HKCU:\\Software\\CentaerisInstallerTest-${randomUUID()}`;
  await fs.writeFile(fixture, `param([string]$RegistryRoot, [string]$Mode)
$ErrorActionPreference = 'Stop'
if ($RegistryRoot -notmatch '^HKCU:\\\\Software\\\\CentaerisInstallerTest-[a-f0-9-]{36}$') { throw 'Invalid fixture root' }
if ($Mode -eq 'remove') {
    if (Test-Path -LiteralPath $RegistryRoot) { Remove-Item -LiteralPath $RegistryRoot -Recurse -Force }
    return
}
New-Item -Path $RegistryRoot -Force | Out-Null
if ($Mode -eq 'populate') {
    $app = New-Item -Path ($RegistryRoot + '\\desktop') -Force
    $app | New-ItemProperty -Name DisplayName -Value 'Centaeris Desktop' | Out-Null
    $app | New-ItemProperty -Name DisplayVersion -Value '0.1.0' | Out-Null
    $app | New-ItemProperty -Name UninstallString -Value '"C:\\Apps\\Centaeris Desktop\\Uninstall Centaeris Desktop.exe" /currentuser' | Out-Null
    $other = New-Item -Path ($RegistryRoot + '\\other') -Force
    $other | New-ItemProperty -Name DisplayName -Value 'Other application' | Out-Null
}
`);
  const fixtureStep = mode => {
    const result = run(fixture, ["-RegistryRoot", registryRoot, "-Mode", mode]);
    assert.equal(result.status, 0, result.stdout + result.stderr);
  };
  try {
    fixtureStep("empty");
    const empty = query(registryRoot);
    assert.equal(empty.status, 0, empty.stdout + empty.stderr);
    assert.equal(empty.stdout.trim(), "");
    fixtureStep("populate");
    const installed = query(registryRoot);
    assert.equal(installed.status, 0, installed.stdout + installed.stderr);
    assert.deepEqual(JSON.parse(installed.stdout), {
      DisplayName: "Centaeris Desktop",
      DisplayVersion: "0.1.0",
      UninstallString: '"C:\\Apps\\Centaeris Desktop\\Uninstall Centaeris Desktop.exe" /currentuser',
    });
  } finally {
    fixtureStep("remove");
    const root = await fs.realpath(directory);
    const temporary = await fs.realpath(os.tmpdir());
    assert.ok(root.startsWith(temporary + path.sep));
    await fs.rm(root, { recursive: true, force: true });
  }
});

windowsTest("installer registration reports registry query errors", () => {
  const result = query("CentaerisMissingRegistryDrive:\\Uninstall");
  assert.notEqual(result.status, 0);
  assert.ok(result.stderr.trim());
});
