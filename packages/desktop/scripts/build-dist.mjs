import fs from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import * as ResEdit from "resedit";
import { writeThirdPartyLicenses } from "./third-party-licenses.mjs";
import { inspectSystemSkillsBundle } from "../src/systemSkills.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");
const electronDist = path.join(hostRoot, "node_modules", "electron", "dist");
const uiDist = path.join(repoRoot, "packages", "ui", "dist");
const runtimeExecutable = path.join(
  repoRoot,
  "target",
  "release",
  "centaeris-runtime.exe",
);
const trayIconIco = path.join(hostRoot, "assets", "icon.ico");
const trayIconIcns = path.join(hostRoot, "assets", "icon.icns");
const outRoot = path.join(hostRoot, "dist");
const appRoot = path.join(outRoot, "Centaeris Desktop");
const resourcesRoot = path.join(appRoot, "resources");
const systemSkillsSource = process.env.CENTAERIS_SYSTEM_SKILLS_SOURCE?.trim();
const systemSkillsBundle = systemSkillsSource
  ? await inspectSystemSkillsBundle(systemSkillsSource)
  : null;
const packagedAppRoot = path.join(resourcesRoot, "app");
const keptLocales = ["en-US", "zh-CN", "zh-TW", "ja", "ko"];

const pathExists = async (target) => {
  try {
    await fs.access(target);
    return true;
  } catch {
    return false;
  }
};

const requirePath = async (target, label) => {
  if (!(await pathExists(target))) {
    throw new Error(`${label} does not exist: ${target}`);
  }
};

const ensureReleaseRuntime = () => {
  const result = spawnSync(
    process.execPath,
    [path.join(hostRoot, "scripts", "ensure-runtime.mjs"), "--profile", "release"],
    { cwd: repoRoot, stdio: "inherit", shell: false },
  );
  if (result.error) {
    throw new Error(`ensure-runtime failed to start: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`ensure-runtime exited with code ${result.status}`);
  }
};

await requirePath(electronDist, "Electron runtime");
await requirePath(path.join(uiDist, "index.html"), "UI dist");
ensureReleaseRuntime();
await requirePath(trayIconIco, "Tray icon (ico)");

await fs.rm(outRoot, { recursive: true, force: true });
await fs.mkdir(appRoot, { recursive: true });
await fs.cp(electronDist, appRoot, { recursive: true });

await fs.rm(path.join(appRoot, "resources", "default_app.asar"), {
  force: true,
});

await fs.copyFile(
  path.join(repoRoot, "LICENSE"),
  path.join(appRoot, "LICENSE.centaeris.txt"),
);
await fs.copyFile(
  path.join(repoRoot, "COPYRIGHT"),
  path.join(appRoot, "COPYRIGHT"),
);
const distNotices = (await fs.readFile(path.join(repoRoot, "THIRD_PARTY_NOTICES.md"), "utf8"))
  .replaceAll("packages/ui/public/licenses/", "resources/ui-dist/licenses/");
await fs.writeFile(path.join(appRoot, "THIRD_PARTY_NOTICES.md"), distNotices);
const localesRoot = path.join(appRoot, "locales");
for (const entry of await fs.readdir(localesRoot)) {
  const baseName = path.basename(entry, ".pak");
  if (!keptLocales.includes(baseName)) {
    await fs.rm(path.join(localesRoot, entry), { force: true });
  }
}
await fs.mkdir(packagedAppRoot, { recursive: true });
await fs.cp(path.join(hostRoot, "src"), path.join(packagedAppRoot, "src"), {
  recursive: true,
});
await fs.copyFile(
  path.join(hostRoot, "package.json"),
  path.join(packagedAppRoot, "package.json"),
);

await fs.mkdir(path.join(resourcesRoot, "ui-dist"), { recursive: true });
await fs.cp(uiDist, path.join(resourcesRoot, "ui-dist"), { recursive: true });

if (systemSkillsBundle) {
  const packagedSystemSkills = path.join(resourcesRoot, "system-skills");
  await fs.cp(systemSkillsSource, packagedSystemSkills, {
    recursive: true,
  });
  const packagedBundle = await inspectSystemSkillsBundle(packagedSystemSkills);
  if (packagedBundle.digest !== systemSkillsBundle.digest) {
    throw new Error("Packaged System Skill bundle digest mismatch");
  }
  console.log(
    `Bundled ${systemSkillsBundle.skillNames.length} System Skills (${systemSkillsBundle.digest})`,
  );
}

await writeThirdPartyLicenses(
  repoRoot,
  hostRoot,
  path.join(appRoot, "THIRD_PARTY_LICENSES"),
);

await fs.mkdir(path.join(resourcesRoot, "bin"), { recursive: true });
await fs.copyFile(
  runtimeExecutable,
  path.join(resourcesRoot, "bin", "centaeris-runtime.exe"),
);

await fs.copyFile(trayIconIco, path.join(resourcesRoot, "icon.ico"));
if (await pathExists(trayIconIcns)) {
  await fs.copyFile(trayIconIcns, path.join(resourcesRoot, "icon.icns"));
}

const electronExe = path.join(appRoot, "electron.exe");
const centaerisExe = path.join(appRoot, "Centaeris Desktop.exe");
if (await pathExists(centaerisExe)) {
  await fs.rm(centaerisExe, { force: true });
}
await fs.rename(electronExe, centaerisExe);
await customizeExecutable(centaerisExe, trayIconIco);
const licenseCheck = spawnSync(
  process.execPath,
  [
    path.join(hostRoot, "scripts", "check-dist-licenses.mjs"),
    "--dist",
    appRoot,
    ...(systemSkillsSource ? ["--system-skills-source", systemSkillsSource] : []),
  ],
  { cwd: repoRoot, stdio: "inherit", shell: false },
);
if (licenseCheck.error) {
  throw new Error(`dist license check failed to start: ${licenseCheck.error.message}`);
}
if (licenseCheck.status !== 0) {
  throw new Error(`dist license check exited with code ${licenseCheck.status}`);
}

console.log(`Electron directory build created: ${appRoot}`);
console.log(`Executable: ${centaerisExe}`);

async function customizeExecutable(exePath, iconPath) {
  const exeData = await fs.readFile(exePath);
  const iconData = await fs.readFile(iconPath);
  const exe = ResEdit.NtExecutable.from(exeData, { ignoreCert: true });
  const resources = ResEdit.NtExecutableResource.from(exe);
  const iconFile = ResEdit.Data.IconFile.from(iconData);
  const iconGroups = ResEdit.Resource.IconGroupEntry.fromEntries(resources.entries);
  if (iconGroups.length === 0) {
    throw new Error(`Executable has no icon resource group: ${exePath}`);
  }
  for (const group of iconGroups) {
    ResEdit.Resource.IconGroupEntry.replaceIconsForResource(
      resources.entries,
      group.id,
      group.lang,
      iconFile.icons.map((item) => item.data),
    );
  }
  const versionInfos = ResEdit.Resource.VersionInfo.fromEntries(resources.entries);
  if (versionInfos.length === 0) {
    throw new Error(`Executable has no version info: ${exePath}`);
  }
  const executableName = path.basename(exePath);
  const productName = path.basename(exePath, path.extname(exePath));
  for (const versionInfo of versionInfos) {
    const languages = versionInfo.getAllLanguagesForStringValues();
    if (languages.length === 0) {
      throw new Error(`Executable has no version string language: ${exePath}`);
    }
    for (const language of languages) {
      versionInfo.setStringValues(language, {
        FileDescription: productName,
        InternalName: executableName,
        OriginalFilename: executableName,
        ProductName: productName,
      });
    }
    versionInfo.outputToResourceEntries(resources.entries);
  }
  resources.outputResource(exe);
  const replacedBinary = Buffer.from(exe.generate());
  await fs.writeFile(exePath, replacedBinary);
}
