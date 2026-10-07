import assert from 'node:assert/strict';
import test from 'node:test';
import {parseSourceRevision, sourceRevision, verifySourceCheckout} from './source-revision.mjs';
const sha = 'a'.repeat(40);
test('accepts the complete SHA and rejects absent, malformed and multiple identities', () => {
  assert.equal(parseSourceRevision(sha + '\r\n'), sha);
  for (const value of ['', sha.slice(0,7), 'z'.repeat(40), sha+'\n'+sha]) assert.throws(() => parseSourceRevision(value));
});
test('provenance uses actual checkout HEAD, not an event or external pin', () => {
  const run = (command,args) => { assert.equal(command,'git'); assert(args.includes('HEAD')); return sha+'\n'; };
  assert.equal(sourceRevision(run,'root with spaces'),sha);
});
test('publication provenance requires clean tracked source', () => {
  assert.equal(verifySourceCheckout((_command,args) => args.includes('rev-parse') ? sha : ''),sha);
  assert.throws(() => verifySourceCheckout((_command,args) => args.includes('rev-parse') ? sha : ' M Cargo.toml'),/tracked modifications/);
  assert.throws(() => sourceRevision(() => {throw new Error('git unavailable');}),/git unavailable/);
});
