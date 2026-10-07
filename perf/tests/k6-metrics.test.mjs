import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

test('replayed terminals are exact and the stream request has an overall timeout', () => {
  const source = fs.readFileSync(new URL('../k6/scenarios/common.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  for (const status of ['completed', 'failed', 'interrupted']) {
    const context = vm.createContext({ __ENV: {}, Date, http: { get(url, options) {
      assert.ok(url.endsWith('/sessions/s/agent-runs/r/events'));
      assert.equal(options.timeout, '120s');
      return { status: 200, body: ': heartbeat\n\ndata: ' + JSON.stringify({ event: { type: 'agent_run_' + status } }) + '\n\n' };
    } } });
    const result = vm.runInContext(source + '\nwaitForTerminal({}, "csrf", "s", "r", 120)', context);
    assert.equal(result.status, status);
  }
});

test('each submitted run contributes a success or failure rate sample', () => {
  const source = fs.readFileSync(new URL('../k6/scenarios/s2-runs.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  for (const failed of [false, true]) {
    const samples = [];
    class Metric { constructor(name) { this.name = name; } add(value) { if (this.name === 's2_create_errors') samples.push(value); } }
    const context = vm.createContext({ Trend: Metric, Counter: Metric, Rate: Metric, __ENV: {},
      startRun() { if (failed) throw new Error('fixture failure'); return { sessionId: 's', agentRunId: 'r' }; } });
    vm.runInContext(source + '\ncreateRun({});', context);
    assert.deepEqual(samples, [failed ? 1 : 0]);
  }
});
