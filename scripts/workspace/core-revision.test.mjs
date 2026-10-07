import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { parseCoreRevision, resolveCoreRevision } from './resolve-core-revision.mjs';
import { verifyCoreCheckout } from './verify-core-checkout.mjs';

const sha = '0123456789abcdef0123456789abcdef01234567';

test('accepts one exact pinned full SHA', () => {
  assert.equal(parseCoreRevision(`${sha}\n`), sha);
  assert.equal(parseCoreRevision(`${sha}\r\n`), sha);
});

test('rejects absent, abbreviated, malformed, or multiple revisions', () => {
  for (const response of ['', `${sha.slice(0, 7)}\n`,
    `${'z'.repeat(40)}\n`, `${sha}\trefs/heads/main\n`,
    `${sha}\n${sha}\n`]) {
    assert.throws(() => parseCoreRevision(response));
  }
});

test('reads the checked-in revision and propagates failures without a fallback', () => {
  let calls = 0;
  assert.equal(resolveCoreRevision(() => { calls++; return `${sha}\n`; }), sha);
  assert.equal(calls, 1);
  assert.throws(() => resolveCoreRevision(() => { throw new Error('file unavailable'); }), /file unavailable/);
});

test('local Core checkout must match the pinned revision', () => {
  const clean = (_command, args) => args.includes('rev-parse') ? `${sha}\n` : '';
  assert.equal(verifyCoreCheckout(clean, sha, 'arbitrary directory'), sha);
  assert.throws(() => verifyCoreCheckout(() => `${'f'.repeat(40)}\n`, sha, 'arbitrary directory'), /does not match/);
  assert.throws(() => verifyCoreCheckout((_command, args) => args.includes('rev-parse') ? sha : ' M Cargo.toml', sha, 'arbitrary directory'), /tracked modifications/);
});

// CI checks out the pinned SHA; this local smoke verifies that it remains fetchable.
test('CLI publishes one publicly fetchable SHA to job output and run summary', {
  skip: process.env.GITHUB_ACTIONS === 'true',
}, () => {
  const directory = mkdtempSync(join(tmpdir(), 'centaeris-core-revision-'));
  try {
    const output = join(directory, 'output');
    const summary = join(directory, 'summary');
    const script = fileURLToPath(new URL('./resolve-core-revision.mjs', import.meta.url));
    const stdout = execFileSync(process.execPath, [script], {
      encoding: 'utf8', timeout: 60_000,
      env: { ...process.env, GITHUB_OUTPUT: output, GITHUB_STEP_SUMMARY: summary },
    }).trim();
    assert.match(stdout, /^[0-9a-f]{40}$/);
    assert.equal(readFileSync(output, 'utf8'), `revision=${stdout}\n`);
    assert.ok(readFileSync(summary, 'utf8').includes(`https://github.com/EchoTrigger/centaeris/commit/${stdout}`));
    const git = (...args) => execFileSync('git', args, {
      cwd: directory, encoding: 'utf8', timeout: 120_000,
      env: { ...process.env, GIT_TERMINAL_PROMPT: '0' },
    }).trim();
    git('init', '--bare', '--quiet');
    git('fetch', '--quiet', '--depth=1', 'https://github.com/EchoTrigger/centaeris.git', stdout);
    assert.equal(git('rev-parse', 'FETCH_HEAD'), stdout);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
