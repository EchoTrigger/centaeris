import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

const licenseName = /^(?:license|licence|copying|copyright|notice)(?:[._-]|$)/i;

const fallbacks = new Map([
  [
    "npm:@radix-ui/react-compose-refs@1.1.2",
    {
      sourcePackage: "@radix-ui/react-slot",
      sourceVersion: "1.2.4",
      file: "LICENSE",
      sha256: "0e80a2d229d2fd4fc7e8636142ec5d0ff0bc031f14c15b682e2ac01dfd5b5138",
    },
  ],
  [
    "npm:@rolldown/binding-win32-x64-msvc@1.1.5",
    {
      sourcePackage: "rolldown",
      sourceVersion: "1.1.5",
      file: "LICENSE",
      sha256: "23ecfff35a5a2e80d92142f75228912c3b1abc4b5a8337a821ff4397e2f9f734",
    },
  ],
  [
    "rust:rmcp@3.1.4",
    {
      commit: "4a738b9dd99eaca418b614afa433a0cbdaf8d056",
      file: "third_party_license_fallbacks/rust/rmcp-3.1.4/LICENSE",
      sha256: "0382b0057770ca05e9c350a50aa3b1c1fea84da0bc81d723bf00b9aa841be58a",
      sourceUrl:
        "https://raw.githubusercontent.com/modelcontextprotocol/rust-sdk/4a738b9dd99eaca418b614afa433a0cbdaf8d056/LICENSE",
    },
  ],
  [
    "rust:tree-sitter@0.25.10",
    {
      commit: "da6fe9beb4f7f67beb75914ca8e0d48ae48d6406",
      file: "third_party_license_fallbacks/rust/tree-sitter-0.25.10/LICENSE",
      sha256: "09b1195d61dd1ff227d38e936040440b205825d3be05176d72e0f19e9899ab1f",
      sourceUrl:
        "https://raw.githubusercontent.com/tree-sitter/tree-sitter/da6fe9beb4f7f67beb75914ca8e0d48ae48d6406/LICENSE",
    },
  ],
  [
    "rust:base64-simd@0.8.0",
    {
      commit: "d74c030d9dc4f3cae02146d1f497ff62726ef09a",
      file: "third_party_license_fallbacks/rust/simd-d74c030d/LICENSE",
      sha256: "14e66de892a0e218a4d60b2cc41a17a28080c46621d812fa2471983d8c524748",
      sourceUrl:
        "https://raw.githubusercontent.com/Nugine/simd/d74c030d9dc4f3cae02146d1f497ff62726ef09a/LICENSE",
    },
  ],
  [
    "rust:vsimd@0.8.0",
    {
      commit: "d74c030d9dc4f3cae02146d1f497ff62726ef09a",
      file: "third_party_license_fallbacks/rust/simd-d74c030d/LICENSE",
      sha256: "14e66de892a0e218a4d60b2cc41a17a28080c46621d812fa2471983d8c524748",
      sourceUrl:
        "https://raw.githubusercontent.com/Nugine/simd/d74c030d9dc4f3cae02146d1f497ff62726ef09a/LICENSE",
    },
  ],
  [
    "rust:clipboard-win@5.4.1",
    {
      commit: "3b27cf2bfd1adcfa6e0264eb51c1025ddaf0f342",
      file: "third_party_license_fallbacks/rust/clipboard-win-5.4.1/LICENSE",
      sha256: "c9bff75738922193e67fa726fa225535870d2aa1059f91452c411736284ad566",
      sourceUrl:
        "https://raw.githubusercontent.com/DoumanAsh/clipboard-win/3b27cf2bfd1adcfa6e0264eb51c1025ddaf0f342/LICENSE",
    },
  ],
]);

const run = (command, args, cwd, shell = false) => {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    shell,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.status}): ${result.stderr}`);
  }
  return result.stdout;
};

const readJson = async (file) => JSON.parse(await fs.readFile(file, "utf8"));
const sha256 = (content) => createHash("sha256").update(content).digest("hex");
const sourceText = (source) =>
  typeof source === "string"
    ? source
    : [source?.url, source?.directory].filter(Boolean).join("#");
const packageDirectory = (ecosystem, name, version) =>
  path.join(ecosystem, `${name.replaceAll("@", "").replaceAll("/", "__")}@${version}`);

const licenseFiles = async (directory) =>
  (await fs.readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && licenseName.test(entry.name))
    .map((entry) => entry.name)
    .sort();

const npmPackages = async (repoRoot) => {
  const paths = run(
    process.platform === "win32" ? "npm.cmd" : "npm",
    ["ls", "--omit=dev", "--all", "--workspace", "centaeris-ui", "--parseable"],
    repoRoot,
    process.platform === "win32",
  )
    .split(/\r?\n/u)
    .filter(Boolean);
  const packages = [];
  for (const directory of paths) {
    const manifestPath = path.join(directory, "package.json");
    try {
      const manifest = await readJson(manifestPath);
      if (!manifest.name || manifest.private || manifest.name === "centaeris-ui") continue;
      packages.push({
        ecosystem: "npm",
        name: manifest.name,
        version: manifest.version,
        license: manifest.license,
        source: sourceText(manifest.repository ?? manifest.homepage ?? manifest._resolved),
        directory,
      });
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  return packages;
};

const rustPackages = async (repoRoot, rustPackageNames) => {
  const metadata = JSON.parse(
    run("cargo", ["metadata", "--locked", "--format-version", "1"], repoRoot),
  );
  const workspace = new Set(metadata.workspace_members);
  const byKey = new Map(
    metadata.packages
      .filter((item) => !workspace.has(item.id))
      .map((item) => [`${item.name}@${item.version}`, item]),
  );
  const keys = new Set();
  for (const rustPackageName of rustPackageNames) {
    const tree = run(
      "cargo",
      [
        "tree",
        "--locked",
        "-p",
        rustPackageName,
        "--target",
        "x86_64-pc-windows-msvc",
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
      ],
      repoRoot,
    );
    for (const line of tree.split(/\r?\n/u)) {
      const match = /^(.+) v([^ ]+)(?: \(.*\))?(?: \(\*\))?$/u.exec(line.trim());
      if (match) keys.add(`${match[1]}@${match[2]}`);
    }
  }
  return [...keys]
    .map((key) => byKey.get(key))
    .filter(Boolean)
    .map((item) => ({
      ecosystem: "rust",
      name: item.name,
      version: item.version,
      license:
        item.license ??
        (item.license_file ? `LicenseRef-${item.name.replaceAll("_", "-")}` : undefined),
      source: item.repository ?? item.homepage,
      directory: path.dirname(item.manifest_path),
    }));
};

const fallbackFiles = async (item, repoRoot, hostRoot) => {
  const key = `${item.ecosystem}:${item.name}@${item.version}`;
  const fallback = fallbacks.get(key);
  if (!fallback) throw new Error(`third-party license files missing: ${key}`);
  let sourcePath;
  let source;
  if (item.ecosystem === "npm") {
    const sourceRoot = path.join(repoRoot, "node_modules", ...fallback.sourcePackage.split("/"));
    const manifest = await readJson(path.join(sourceRoot, "package.json"));
    if (manifest.version !== fallback.sourceVersion) {
      throw new Error(`third-party fallback version mismatch: ${key}`);
    }
    sourcePath = path.join(sourceRoot, fallback.file);
    source = `${fallback.sourcePackage}@${fallback.sourceVersion}/${fallback.file}`;
  } else {
    const vcs = await readJson(path.join(item.directory, ".cargo_vcs_info.json"));
    if (vcs.git?.sha1 !== fallback.commit) {
      throw new Error(`third-party fallback commit mismatch: ${key}`);
    }
    sourcePath = path.join(hostRoot, fallback.file);
    source = fallback.sourceUrl;
  }
  const content = await fs.readFile(sourcePath);
  if (sha256(content) !== fallback.sha256) {
    throw new Error(`third-party fallback digest mismatch: ${key}`);
  }
  return [{
    name: path.basename(sourcePath),
    content,
    source,
    sourceCommit: fallback.commit,
  }];
};

export async function collectThirdPartyLicenses(
  repoRoot,
  hostRoot,
  rustPackageNames = ["centaeris-runtime"],
  includeNpm = true,
) {
  const items = [
    ...(includeNpm ? await npmPackages(repoRoot) : []),
    ...(await rustPackages(repoRoot, rustPackageNames)),
  ];
  const unique = new Map(items.map((item) => [`${item.ecosystem}:${item.name}@${item.version}`, item]));
  const result = [];
  for (const [key, item] of [...unique].sort(([left], [right]) =>
    left < right ? -1 : left > right ? 1 : 0,
  )) {
    if (!item.version || !item.license || !item.source) {
      throw new Error(`third-party package metadata incomplete: ${key}`);
    }
    const names = await licenseFiles(item.directory);
    const files = names.length
      ? await Promise.all(
          names.map(async (name) => ({
            name,
            content: await fs.readFile(path.join(item.directory, name)),
            source: `${item.source}#${name}`,
          })),
        )
      : await fallbackFiles(item, repoRoot, hostRoot);
    result.push({ ...item, files });
  }
  return result;
}

export async function writeThirdPartyLicenses(
  repoRoot,
  hostRoot,
  outputRoot,
  rustPackageNames,
  includeNpm,
) {
  const items = await collectThirdPartyLicenses(
    repoRoot,
    hostRoot,
    rustPackageNames,
    includeNpm,
  );
  const temporaryRoot = `${outputRoot}.tmp-${process.pid}`;
  await fs.rm(temporaryRoot, { recursive: true, force: true });
  await fs.mkdir(temporaryRoot, { recursive: true });
  const index = [];
  try {
    for (const item of items) {
      const relativeRoot = packageDirectory(item.ecosystem, item.name, item.version);
      const destinationRoot = path.join(temporaryRoot, relativeRoot);
      await fs.mkdir(destinationRoot, { recursive: true });
      const files = [];
      for (const file of item.files) {
        const relativePath = path.join(relativeRoot, file.name).replaceAll("\\", "/");
        await fs.writeFile(path.join(temporaryRoot, relativePath), file.content);
        files.push({
          path: relativePath,
          sha256: sha256(file.content),
          source: file.source,
          ...(file.sourceCommit ? { sourceCommit: file.sourceCommit } : {}),
        });
      }
      index.push({
        ecosystem: item.ecosystem,
        name: item.name,
        version: item.version,
        license: item.license,
        source: item.source,
        files,
      });
    }
    await fs.writeFile(
      path.join(temporaryRoot, "index.json"),
      `${JSON.stringify({ schema: "centaeris_third_party_licenses_v1", packages: index }, null, 2)}\n`,
    );
    await fs.rm(outputRoot, { recursive: true, force: true });
    await fs.rename(temporaryRoot, outputRoot);
  } finally {
    await fs.rm(temporaryRoot, { recursive: true, force: true });
  }
  return items.length;
}
