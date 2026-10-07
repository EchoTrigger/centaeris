import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const CORE_PACKAGES = [
  ['centaeris-core', 'core'], ['centaeris-model-catalog', 'model-catalog'],
  ['centaeris-mcp', 'mcp'], ['centaeris-runtime-sqlite', 'runtime_sqlite'],
];
export const workspaceRoot = fileURLToPath(new URL('../../', import.meta.url));

export function coreSourceFromMetadata(metadata, root = workspaceRoot) {
  root = resolve(root);
  if (resolve(metadata.workspace_root) !== root) throw new Error('Cargo workspace must be this monorepo');
  for (const [name, folder] of CORE_PACKAGES) {
    const matches = metadata.packages.filter(p => p.name === name);
    if (matches.length !== 1 || matches[0].source !== null ||
        resolve(matches[0].manifest_path) !== resolve(root, 'packages', folder, 'Cargo.toml')) {
      throw new Error(`${name} must use its owned package in this monorepo`);
    }
  }
  return root;
}

export function resolveCoreSource({ run = execFileSync } = {}) {
  if (process.env.CENTAERIS_CORE_PATH) throw new Error('External Core overrides are unsupported');
  const metadata = JSON.parse(run('cargo', ['metadata', '--locked', '--format-version=1'], {
    cwd: workspaceRoot, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
  }));
  return coreSourceFromMetadata(metadata);
}

let cachedRoot;
export function coreFile(relativePath) {
  cachedRoot ??= resolveCoreSource();
  return resolve(cachedRoot, relativePath);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.length !== 2) throw new Error('Usage: node scripts/workspace/core-source.mjs');
  console.log(resolveCoreSource());
}
