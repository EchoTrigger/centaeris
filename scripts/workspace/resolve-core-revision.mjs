import { appendFileSync, readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function parseCoreRevision(output) {
  const match = /^([0-9a-f]{40})(?:\r?\n)?$/.exec(output);
  if (!match) throw new Error('Expected exactly one pinned full Core SHA');
  return match[1];
}

export function resolveCoreRevision(readRevision = readFileSync) {
  return parseCoreRevision(readRevision(new URL('../core-revision.txt', import.meta.url), 'utf8'));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const revision = resolveCoreRevision();
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `revision=${revision}\n`);
  }
  if (process.env.GITHUB_STEP_SUMMARY) {
    appendFileSync(process.env.GITHUB_STEP_SUMMARY,
      `Pinned Core revision: [${revision}](https://github.com/EchoTrigger/centaeris/commit/${revision})\n`);
  }
  console.log(revision);
}
