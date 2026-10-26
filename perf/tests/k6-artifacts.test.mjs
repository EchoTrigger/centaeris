import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { createHash } from 'node:crypto';
const source = fs.readFileSync(new URL('../k6/scenarios/common.js', import.meta.url), 'utf8')
  .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
const bytes = Buffer.from('independent artifact bytes\n');
const expectation = { filename: 'perf-result.bin', sizeBytes: bytes.length,
  sha256: 'sha256:' + createHash('sha256').update(bytes).digest('hex') };
function scenario({ content = bytes, change } = {}) {
  const events = [
    ['tool_call', { callId: 'bash-call', toolName: 'bash' }],
    ['tool_result', { callId: 'bash-call', toolName: 'bash', resultState: 'successWithOutput' }],
    ['tool_call', { callId: 'publish-call', toolName: 'publish_artifact', normalizedInput: { path: '/mnt/data/perf-result.bin' } }],
    ['artifact_published', { publicationId: 'pub_' + 'a'.repeat(64), artifactRef: 'artifact:artifact-owned', toolCallId: 'publish-call', ...expectation }],
    ['tool_result', { callId: 'publish-call', toolName: 'publish_artifact', resultState: 'successWithOutput', modelContent: 'Published perf-result.bin.' }],
    ['agent_run_completed', {}],
  ].map(([type, payload], index) => ({ type, payload, eventId: `event-${index + 1}`, sequence: index + 1,
    agentRunId: 'run-owned', sessionId: 'session-owned' }));
  const result = { status: 'completed', events };
  change?.(result);
  const calls = [];
  const context = vm.createContext({ __ENV: { API_BASE: 'http://127.0.0.1:18000' }, Date,
    crypto: { sha256(value) { return createHash('sha256').update(Buffer.from(value)).digest('hex'); } }, sleep() {},
    http: { get(url, options) {
      calls.push({ url, options });
      return { status: 200, body: content.buffer.slice(content.byteOffset, content.byteOffset + content.length) };
    } },
  });
  vm.runInContext(source, context);
  return { context, result, calls };
}
test('published artifact is bound to a successful tool call and independently downloaded and hashed', () => {
  const s = scenario();
  const proof = s.context.verifyPublishedArtifact({ sessionid: 'synthetic' }, s.result, 'session-owned', 'run-owned', expectation);
  assert.equal(proof.sha256, expectation.sha256);
  assert.equal(s.calls.length, 1);
  assert.equal(s.calls[0].url, 'http://127.0.0.1:18000/api/artifacts/artifact-owned/download');
  assert.equal(s.calls[0].options.responseType, 'binary');
  assert.equal(s.calls[0].options.redirects, 0);
});
test('matching published metadata does not hide changed download bytes', () => {
  const s = scenario({ content: Buffer.from('changed independent bytes!\n') });
  assert.throws(() => s.context.verifyPublishedArtifact({}, s.result, 'session-owned', 'run-owned', expectation), /download.*(hash|size)/);
});
test('foreign publication call, failed publish or untrusted metadata is rejected before download', () => {
  for (const change of [
    value => { value.events[3].payload.toolCallId = 'other'; },
    value => { value.events[4].payload.resultState = 'failed'; },
    value => { value.events[3].payload.sha256 = 'sha256:' + 'b'.repeat(64); },
    value => { value.events[3].agentRunId = 'foreign'; },
    value => { value.events.splice(3, 1); },
  ]) {
    const s = scenario({ change });
    assert.throws(() => s.context.verifyPublishedArtifact({}, s.result, 'session-owned', 'run-owned', expectation));
    assert.equal(s.calls.length, 0);
  }
});
