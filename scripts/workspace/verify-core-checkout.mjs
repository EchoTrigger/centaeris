import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { parseCoreRevision, resolveCoreRevision } from './resolve-core-revision.mjs';
import { resolveCoreSource } from './core-source.mjs';

export function verifyCoreCheckout(runGit = execFileSync, expected = resolveCoreRevision(), coreRoot = resolveCoreSource({ allowLocal: false })) {
  const actual = parseCoreRevision(runGit('git', ['-C', coreRoot, 'rev-parse', 'HEAD'], {
    encoding: 'utf8', timeout: 30_000,
  }));
  if (actual !== expected) {
    throw new Error(`Local Core checkout ${actual} does not match pinned ${expected}`);
  }
  if (runGit('git', ['-C', coreRoot, 'status', '--porcelain', '--untracked-files=no'], { encoding: 'utf8' }).trim()) {
    throw new Error('Pinned Core source has tracked modifications');
  }
  return actual;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  console.log(verifyCoreCheckout());
}
