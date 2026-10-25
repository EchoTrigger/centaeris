import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('../k6/scenarios/e2b-main-path.js', import.meta.url));
const binary = process.env.K6_BINARY || fileURLToPath(new URL(`../k6/bin/k6${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
for (const failDurable of [false, true]) {
  test(`k6 owns serial lifecycle cases and finish reporting: durable failure=${failDurable}`, async context => {
    if (!fs.existsSync(binary)) return context.skip('Set K6_BINARY to the pinned xk6-sse binary');
    const admittedSessions = [];
    const runs = new Map();
    let finished = null;
    let configuration;
    const server = http.createServer((request, response) => {
      const parsed = new URL(request.url, 'http://localhost');
      const json = value => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(value)); };
      if (parsed.pathname === '/config') return json(configuration);
      if (parsed.pathname === '/state') {
        if (parsed.searchParams.has('runId')) return json({ runStatus: 'completed', jobStatus: failDurable ? 'failed' : 'succeeded' });
        const role = parsed.searchParams.get('sessionId') === 'session_A' ? 'A' : 'B';
        const count = admittedSessions.length;
        let running;
        if (count <= 3) running = 'A';
        else if (count <= 7) running = null;
        else running = count % 2 === 0 ? 'B' : 'A';
        return json({ powerState: running === role ? 'running' : 'paused' });
      }
      if (parsed.pathname === '/finish') {
        let body = '';
        request.on('data', chunk => { body += chunk; });
        request.on('end', () => { finished = JSON.parse(body); json({ ok: true }); });
        return;
      }
      if (request.method === 'POST') {
        assert.equal(request.headers.cookie, 'suite-cookie-private');
        const requested = parsed.pathname.split('/')[5];
        assert.equal(parsed.pathname, `/api/workspaces/workspace_fixture/sessions/${requested}/messages`);
        const session = requested === 'new' ? (admittedSessions.length === 0 ? 'session_A' : 'session_B') : requested;
        const id = 'agent_run_' + (admittedSessions.length + 1);
        runs.set(id, session);
        admittedSessions.push(session);
        request.resume();
        response.writeHead(202, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify({ sessionId: session, agentRunId: id }));
        return;
      }
      const runId = parsed.pathname.split('/')[5];
      assert.ok(runs.has(runId), parsed.pathname);
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      const event = (type, sequence, payload = {}) => ({ schema: 'session.stream.item.v1', kind: 'committed', agentRunId: runId,
        sourceSequence: sequence, event: { schemaVersion: 'session.event.v1', eventVersion: 1, agentRunId: runId,
          sessionId: runs.get(runId), type, sequence, payload } });
      response.write('data: ' + JSON.stringify(event('tool_result', 1, { resultState: 'successNoOutput' })) + '\n\n');
      response.end('data: ' + JSON.stringify(event('agent_run_completed', 2)) + '\n\n');
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    context.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
    const url = `http://127.0.0.1:${server.address().port}`;
    configuration = { API_BASE: url, PERF_WORKSPACE_ID: 'workspace_fixture', PERF_AGENT_ID: 'agent_fixture', PERF_MODEL_ID: 'model_fixture',
      PERF_COOKIE_HEADER: 'suite-cookie-private', PERF_CSRF_TOKEN: 'suite-csrf-private', PERF_TIMEOUT_MS: '2000' };
    const result = await new Promise((resolve, reject) => {
      const child = spawn(binary, ['--log-format', 'raw', 'run', '--quiet', '--no-summary', script], {
        env: { ...process.env, PERF_FIXTURE_URL: url, K6_NO_USAGE_REPORT: 'true', NO_PROXY: '*', no_proxy: '*',
          HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', http_proxy: '', https_proxy: '', all_proxy: '' },
      });
      let output = '';
      child.stdout.on('data', chunk => { output += chunk; }); child.stderr.on('data', chunk => { output += chunk; });
      const timer = setTimeout(() => { child.kill(); reject(new Error('Suite fixture exceeded 15 seconds')); }, 15000);
      child.once('error', error => { clearTimeout(timer); reject(error); });
      child.once('close', code => { clearTimeout(timer); resolve({ code, output }); });
    });
    assert.ok(!result.output.includes('suite-cookie-private') && !result.output.includes('suite-csrf-private'));
    assert.ok(finished, result.output);
    assert.equal(finished.passed, !failDurable);
    assert.equal(result.output.split(/\r?\n/).filter(line => line.startsWith('MAIN_PATH_SUITE ')).length, 1);
    if (failDurable) {
      assert.notEqual(result.code, 0);
      assert.equal(admittedSessions.length, 1, 'A failed durable completion must stop further admissions');
      assert.equal(finished.errorCode, 'durable_run_failed');
      assert.equal(finished.reports.length, 1, 'Already accepted Run remains in failure report');
    } else {
      assert.equal(result.code, 0, result.output);
      assert.deepEqual(admittedSessions, [...Array(7).fill('session_A'), 'session_B', 'session_A', 'session_B', 'session_A', 'session_B', 'session_A', 'session_B']);
      assert.deepEqual(finished.reports.map(row => row.case), ['coldA', 'warmA1', 'warmA2', 'warmA3', 'pausedFreeA1', 'pausedFreeA2', 'pausedFreeA3',
        'coldB', 'capacityA1', 'capacityB2', 'capacityA3', 'capacityB4', 'capacityA5', 'capacityB6']);
      assert.ok(finished.reports.every(row => typeof row.preconditionMs === 'number' && typeof row.durableWaitMs === 'number'));
    }
  });
}
