import { execFileSync } from 'node:child_process';
import { appendFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
export const sourceRoot = fileURLToPath(new URL('../../', import.meta.url));
export function parseSourceRevision(value) {
  const revision = value.trim();
  if (!/^[0-9a-f]{40}$/.test(revision)) throw new Error('Expected one complete checkout SHA');
  return revision;
}
export function sourceRevision(run = execFileSync, root = sourceRoot) {
  return parseSourceRevision(run('git', ['-C', root, 'rev-parse', 'HEAD'], {encoding: 'utf8'}));
}
export function verifySourceCheckout(run = execFileSync, root = sourceRoot) {
  const revision = sourceRevision(run, root);
  if (run('git', ['-C', root, 'status', '--porcelain', '--untracked-files=no'], {encoding: 'utf8'}).trim()) {
    throw new Error('Source checkout has tracked modifications');
  }
  return revision;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const revision = verifySourceCheckout();
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `revision=${revision}\n`);
  console.log(revision);
}
