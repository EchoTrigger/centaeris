import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const source = fs.readFileSync(new URL('../k6/scenarios/common.js', import.meta.url), 'utf8')
  .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
function common(environment = {}) {
  const calls = [];
  const context = vm.createContext({ __ENV: environment, Date, Uint8Array,
    crypto: { randomBytes: length => new ArrayBuffer(length) }, sleep() {},
    http: { request(method, url, body) { calls.push({ method, url, body }); throw new Error('unexpected request'); } },
  });
  vm.runInContext(source, context);
  return { context, calls };
}
const configured = { API_BASE: 'http://127.0.0.1:18000', PERF_WORKSPACE_ID: 'workspace-owned',
  PERF_AGENT_ID: 'agent-owned', PERF_MODEL_REFS: JSON.stringify({ short: 'model-short', tools: 'model-tools', long: 'model-long' }) };

test('explicit workload target selects only caller-provided workspace and model IDs without HTTP writes', () => {
  const { context, calls } = common(configured);
  const target = context.configuredTarget('tools');
  assert.deepEqual(JSON.parse(JSON.stringify(target)), {
    workspaceId: 'workspace-owned', agentId: 'agent-owned', modelId: 'model-tools' });
  assert.deepEqual(calls, []);
});

test('missing or malformed target fails before network access', () => {
  for (const change of [ { PERF_WORKSPACE_ID: '' }, { PERF_AGENT_ID: '' }, { PERF_MODEL_REFS: '{}' },
    { PERF_MODEL_REFS: '{invalid' }, { PERF_MODEL_REFS: JSON.stringify({ tools: 9 }) }, { API_BASE: '' } ]) {
    const { context, calls } = common({ ...configured, ...change });
    assert.throws(() => context.configuredTarget('tools'), /invalid explicit workload target/);
    assert.deepEqual(calls, []);
  }
});

test('rolling workload reuses the specified session and pre-generated operation identity', () => {
  const { context } = common(configured); let submitted;
  context.http.request = (method, url, body) => {
    submitted = { method, url, payload: JSON.parse(body) };
    return { status: 202, json() { return { sessionId: 'retained-session', agentRunId: 'run-owned' }; } };
  };
  context.startRun({}, 'csrf', 'workspace-owned', 'agent-owned', 'model-tools', 'fixed work',
    { sessionId: 'retained-session', operationId: 'a'.repeat(32), sampleId: 'b'.repeat(32) });
  assert.ok(submitted.url.endsWith('/sessions/retained-session/messages'));
  assert.equal(submitted.payload.operationId, 'a'.repeat(32));
});
