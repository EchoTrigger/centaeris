import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { runtimeArtifactPath } from "../src/runtimeArtifact.mjs";
import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createRuntimeHostTransport } from "../src/runtimeHostTransport.mjs";

if (process.platform !== "win32") throw new Error("Installer acceptance requires Windows");
const installer = process.argv[2];
if (!installer || process.argv.length !== 3) throw new Error("Use smoke-installer.mjs <setup.exe>");
const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "../..");
const digest = async file => createHash("sha256").update(await fs.readFile(file)).digest("hex");
const manifest = JSON.parse(await fs.readFile(path.join(hostRoot, "package.json"), "utf8"));
const powershell = path.join(process.env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe");
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

const run = (executable, args, options = {}) => new Promise((resolve, reject) => {
  const child = spawn(executable, args, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"], ...options });
  let output = "";
  child.stdout.on("data", chunk => { output += chunk; });
  child.stderr.on("data", chunk => { output += chunk; });
  const timeout = setTimeout(() => { child.kill(); reject(new Error("Acceptance child timed out: " + executable)); }, 120_000);
  child.once("error", error => { clearTimeout(timeout); reject(error); });
  child.once("close", code => { clearTimeout(timeout); resolve({ code, output }); });
});
const installedEntry = async () => {
  const result = await run(powershell, ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
    path.join(import.meta.dirname, "installer-registration.ps1")]);
  assert.equal(result.code, 0, result.output);
  return result.output.trim() ? JSON.parse(result.output) : null;
};
assert.equal(await installedEntry(), null, "Installer acceptance requires no existing installed Desktop; portable previews are preserved");
const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-installer-smoke-"));
const installDirectory = path.join(root, "项目's Desktop");
const runtimeData = path.join(root, "profile/.centaeris");
await fs.mkdir(runtimeData, { recursive: true });
const sentinel = path.join(runtimeData, "acceptance-data.txt");
await fs.writeFile(sentinel, "preserve installed user data");
const environment = { ...process.env, CENTAERIS_DESKTOP_DATA_DIR: runtimeData };
delete environment.ELECTRON_RUN_AS_NODE;
const install = () => run(path.resolve(installer), ["/S", "/currentuser", "/D=" + installDirectory], {
  windowsVerbatimArguments: true, env: environment,
});
let transport;
let installed = false;
let uninstallerCopy;
const uninstall = async () => {
  const source = path.join(installDirectory, "Uninstall Centaeris Desktop.exe");
  uninstallerCopy = path.join(root, "uninstall-acceptance.exe");
  await fs.copyFile(source, uninstallerCopy);
  const result = await run(uninstallerCopy, ["/S", "/currentuser", "_?=" + installDirectory], {
    windowsVerbatimArguments: true, env: environment,
  });
  assert.equal(result.code, 0, result.output);
  installed = false;
  assert.equal(await installedEntry(), null, "Uninstall entry must be removed");
  assert.equal(await fs.readFile(sentinel, "utf8"), "preserve installed user data");
};
try {
  console.log("[installer-smoke] install into a path containing Chinese, spaces, and an apostrophe");
  let result = await install();
  assert.equal(result.code, 0, result.output);
  installed = true;
  const entry = await installedEntry();
  assert.equal(entry.DisplayVersion, manifest.version);
  assert.equal(entry.UninstallString, '"' + path.join(installDirectory, "Uninstall Centaeris Desktop.exe") + '" /currentuser');
  const applicationExe = path.join(installDirectory, "Centaeris Desktop.exe");
  // Use an argument-bearing script for paths rather than interpolating shell text.
  const versionScript = path.join(root, "version.ps1");
  await fs.writeFile(versionScript, "param([string]$File)\n(Get-Item -LiteralPath $File).VersionInfo.ProductVersion\n");
  const pe = await run(powershell, ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", versionScript, "-File", applicationExe]);
  assert.equal(pe.code, 0, pe.output);
  assert.equal(pe.output.trim(), manifest.version + ".0");
  assert.equal(
    await digest(path.join(installDirectory, "resources/bin/centaeris-runtime.exe")),
    await digest(runtimeArtifactPath(repoRoot, "release")),
    "Installed Runtime must match the source build",
  );
  const uiRoot = path.join(repoRoot, "packages/ui/dist");
  for (const relative of await fs.readdir(uiRoot, { recursive: true })) {
    if (!(await fs.stat(path.join(uiRoot, relative))).isFile()) continue;
    assert.equal(await digest(path.join(installDirectory, "resources/ui-dist", relative)), await digest(path.join(uiRoot, relative)), "Installed UI must match the accepted source build: " + relative);
  }
  await fs.writeFile(path.join(installDirectory, "old-installation-marker.txt"), "remove during replacement");

  transport = createRuntimeHostTransport({
    executablePath: path.join(installDirectory, "resources/bin/centaeris-runtime.exe"),
    cwd: installDirectory,
    environment, emitHostEvent() {}, isSmokeRun: true,
  });
  await transport.invokeCommand("initialize", { request: { clientKind: "tui", viewerId: "installer-acceptance" } });
  const workspace = path.join(root, "workspace");
  await fs.mkdir(workspace);
  const persistedSession = await transport.invokeCommand("session/new", {
    request: { operationId: "installer-persistence", title: "Retain through reinstall", cwd: workspace },
  });
  await transport.invokeCommand("agent_runtime_config_set", { request: { toolParallelism: 7 } });
  console.log("[installer-smoke] refuse replacement while the installed Runtime has a client");
  result = await install();
  assert.equal(result.code, 2, "Busy replacement must stop before removing the current app: " + result.output);
  await transport.invokeCommand("initialize", { request: { clientKind: "tui", viewerId: "installer-acceptance" } });
  assert.equal(await fs.readFile(path.join(installDirectory, "old-installation-marker.txt"), "utf8"), "remove during replacement");
  await transport.requestAppExit();
  transport = null;
  await delay(8_000);

  console.log("[installer-smoke] replace the installation after normal idle exit");
  result = await install();
  assert.equal(result.code, 0, result.output);
  assert.equal(await fs.stat(path.join(installDirectory, "old-installation-marker.txt")).then(() => true, () => false), false);
  assert.equal(await fs.readFile(sentinel, "utf8"), "preserve installed user data");

  console.log("[installer-smoke] launch installed Desktop and verify session/config persistence");
  result = await run(process.execPath, [path.join(import.meta.dirname, "smoke-window.mjs")], {
    env: { ...environment, CENTAERIS_ELECTRON_SMOKE_EXE: applicationExe },
  });
  assert.equal(result.code, 0, result.output);
  console.log(result.output);

  console.log("[installer-smoke] uninstall and reinstall while retaining user data");
  await uninstall();
  result = await install();
  assert.equal(result.code, 0, result.output);
  installed = true;
  assert.equal(await fs.readFile(sentinel, "utf8"), "preserve installed user data");
  transport = createRuntimeHostTransport({
    executablePath: path.join(installDirectory, "resources/bin/centaeris-runtime.exe"),
    cwd: installDirectory, environment, emitHostEvent() {}, isSmokeRun: true,
  });
  await transport.invokeCommand("initialize", { request: { clientKind: "tui", viewerId: "installer-reinstall" } });
  const sessions = await transport.invokeCommand("session/list", { request: {} });
  assert.ok(sessions.some(session => session.id === persistedSession.id), "Reinstall must retain stored sessions");
  const config = await transport.invokeCommand("agent_runtime_config_get", { request: {} });
  assert.equal(config.toolParallelism, 7, "Reinstall must retain user configuration");
  await transport.requestAppExit();
  transport = null;
  await delay(8_000);
  await uninstall();
  console.log(JSON.stringify({ ok: true, checked: ["install", "perUserRegistration", "applicationVersion", "busyRuntimePreserved", "replacement", "installedWindow", "sessionPersistence", "uninstall", "reinstall", "userDataRetained", "unicodePath"] }));
} finally {
  if (transport) { await transport.requestAppExit(); await delay(8_000); }
  if (installed) await uninstall();
  const resolvedRoot = await fs.realpath(root);
  const resolvedTemp = await fs.realpath(os.tmpdir());
  assert.ok(resolvedRoot.startsWith(resolvedTemp + path.sep));
  await fs.rm(root, { recursive: true, force: true });
}
