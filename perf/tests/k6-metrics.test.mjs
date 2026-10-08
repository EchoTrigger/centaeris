import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

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

test('real K6 rejects actual S2 submission errors without a scenario system tag', async (context) => {
  const binary = fileURLToPath(new URL(`../k6/bin/k6${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
  if (!fs.existsSync(binary)) return context.skip('Build the pinned local K6 binary to exercise threshold enforcement');
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'centaeris-k6-threshold-'));
  context.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  let requests = 0;
  const server = http.createServer((request, response) => {
    assert.equal(request.method, 'POST');
    assert.equal(request.url, '/api/workspaces/fixture/sessions/new/messages');
    request.resume();
    const failed = ++requests % 2 === 0;
    response.writeHead(failed ? 429 : 202, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify(failed ? { error: 'workspace_execution_queue_full' } : { sessionId: 'fixture-session', agentRunId: 'fixture-run' }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  context.after(() => new Promise(resolve => server.close(resolve)));
  const script = path.join(temporary, 'fixture.js');
  const summary = path.join(temporary, 'summary.json');
  const subject = pathToFileURL(fileURLToPath(new URL('../k6/scenarios/s2-runs.js', import.meta.url))).href;
  fs.writeFileSync(script, `import { options as sourceOptions, createRun } from ${JSON.stringify(subject)};
export const options = { ...sourceOptions, scenarios: { runs: { executor: 'shared-iterations', vus: 1, iterations: 4, exec: 'fixtureRun' } } };
export function fixtureRun() { createRun({ store: {}, csrf: 'fixture', workspaceId: 'fixture', agentId: 'fixture', modelId: 'fixture' }); }
`);
  const result = await new Promise((resolve, reject) => {
    const child = spawn(binary, ['run', '--summary-export', summary, script], {
      env: { ...process.env, API_BASE: `http://127.0.0.1:${server.address().port}`, K6_NO_USAGE_REPORT: 'true' },
    });
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.stderr.on('data', chunk => { output += chunk; });
    const timer = setTimeout(() => { child.kill(); reject(new Error('Fixture K6 exceeded 30 seconds')); }, 30000);
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', code => { clearTimeout(timer); resolve({ code, output }); });
  });
  assert.equal(requests, 4);
  const metrics = JSON.parse(fs.readFileSync(summary, 'utf8')).metrics;
  assert.equal(metrics.s2_create_errors.value, 0.5, 'The real scenario must record both rejected submissions');
  assert.notEqual(result.code, 0, `Actual 50% submission errors must fail the K6 run: ${result.output}`);
  assert.equal(metrics.s2_create_errors.thresholds['rate<0.01'], true, 'The threshold must evaluate the actual error-rate series');
});
