import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { CORE_PACKAGES, CORE_URL, workspaceRoot } from './core-source.mjs';

export function patchConfiguration(corePath) {
  const root = resolve(corePath);
  for (const [, folder] of CORE_PACKAGES) {
    if (!existsSync(join(root, 'packages', folder, 'Cargo.toml'))) throw new Error(`Missing Core crate: ${folder}`);
  }
  return `# Local development only. Do not commit this file or the patched Cargo.lock.\n[patch.${JSON.stringify(CORE_URL)}]\n` +
    CORE_PACKAGES.map(([name, folder]) => `${name} = { path = ${JSON.stringify(join(root, 'packages', folder).replaceAll('\\', '/'))} }\n`).join('');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.length !== 3) throw new Error('Usage: node scripts/core-dev.mjs <Core checkout>');
  const config = patchConfiguration(process.argv[2]);
  mkdirSync(join(workspaceRoot, '.cargo'), { recursive: true });
  writeFileSync(join(workspaceRoot, '.cargo/config.toml'), config, { flag: 'wx' });
  console.log('Local patch created. Set CENTAERIS_CORE_PATH to the same checkout, then run cargo check to resolve the development lockfile. Release gates reject local patches.');
}
