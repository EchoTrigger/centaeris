import { execFileSync } from 'node:child_process';
import { resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { resolveCoreRevision } from './resolve-core-revision.mjs';

export const CORE_URL = 'https://github.com/EchoTrigger/centaeris.git';
export const CORE_PACKAGES = [
  ['centaeris-core', 'core'], ['centaeris-model-catalog', 'model-catalog'],
  ['centaeris-mcp', 'mcp'], ['centaeris-runtime-sqlite', 'runtime_sqlite'],
];
export const workspaceRoot = fileURLToPath(new URL('../', import.meta.url));

export function coreSourceFromMetadata(metadata, revision, localPath) {
  const source = `git+${CORE_URL}?rev=${revision}#${revision}`;
  let root;
  for (const [name, folder] of CORE_PACKAGES) {
    const matches = metadata.packages.filter(p => p.name === name);
    if (matches.length !== 1) throw new Error(`Expected exactly one ${name}`);
    const pkg = matches[0];
    const candidate = resolve(dirname(pkg.manifest_path), '../..');
    if (pkg.source !== (localPath ? null : source) ||
        resolve(pkg.manifest_path) !== resolve(candidate, 'packages', folder, 'Cargo.toml') ||
        (root && candidate !== root) || (localPath && candidate !== resolve(localPath))) {
      throw new Error(`${name} does not match the pinned Core source or explicit local override`);
    }
    root = candidate;
  }
  return root;
}

let cachedRoot;
export function resolveCoreSource({ allowLocal = true, run = execFileSync } = {}) {
  const localPath = process.env.CENTAERIS_CORE_PATH;
  if (localPath && (!allowLocal || process.env.GITHUB_ACTIONS === 'true')) {
    throw new Error('Local Core overrides are forbidden in the release/CI gate');
  }
  const metadata = JSON.parse(run('cargo', ['metadata', '--locked', '--format-version=1'], {
    cwd: workspaceRoot, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
  }));
  return coreSourceFromMetadata(metadata, resolveCoreRevision(), localPath);
}

export function coreFile(relativePath) {
  cachedRoot ??= resolveCoreSource();
  return resolve(cachedRoot, relativePath);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  if (args.length && (args.length !== 1 || args[0] !== '--strict')) throw new Error('Usage: node scripts/core-source.mjs [--strict]');
  console.log(resolveCoreSource({ allowLocal: args[0] !== '--strict' }));
}
