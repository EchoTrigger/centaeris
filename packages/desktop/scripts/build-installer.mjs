import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { build, Platform, Arch } from "electron-builder";
import configuration from "../electron-builder.config.mjs";
import { validateInstallerInput } from "./installer-input.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const args = process.argv.slice(2);
const options = {};
for (let index = 0; index < args.length; index += 2) {
  const key = args[index];
  if (!["--app-dir", "--output"].includes(key) || !args[index + 1] || options[key]) {
    throw new Error("Use build-installer.mjs [--app-dir <application-directory>] [--output <installer-directory>]");
  }
  options[key] = args[index + 1];
}
if (process.platform !== "win32" || process.arch !== "x64") throw new Error("Desktop installer builds require Windows x64");
const application = path.resolve(options["--app-dir"] ?? path.join(hostRoot, "dist/Centaeris Desktop"));
const output = path.resolve(options["--output"] ?? path.join(os.tmpdir(), "centaeris-installers"));
if (output === application || output.startsWith(application + path.sep)) throw new Error("Installer output must be outside the application payload");
const manifest = JSON.parse(await fs.readFile(path.join(hostRoot, "package.json"), "utf8"));
await validateInstallerInput(application, manifest.version);
const files = await build({
  projectDir: hostRoot,
  prepackaged: application,
  targets: Platform.WINDOWS.createTarget("nsis", Arch.x64),
  config: { ...configuration, directories: { ...configuration.directories, output }, electronVersion: manifest.devDependencies.electron },
  publish: "never",
});
for (const file of files) console.log(file);
