import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('../k6/scenarios/e2b-main-path.js', import.meta.url));
const binary = process.env.K6_BINARY || fileURLToPath(new URL(`../k6/bin/k6${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));

for (const mode of ['completed', 'waiting65', 'changedFirst', 'changedRuntime']) {
  test(`paused registry keeps 65 distinct environments and restores first: ${mode}`, async context => {
    if (!fs.existsSync(binary)) return context.skip('Set K6_BINARY to the pinned xk6-sse binary');
    const admissions = [], runs = new Map(), paused = new Set();
    let finished, configuration;
    const server = http.createServer((request, response) => {
      const path = new URL(request.url, 'http://localhost');
      const json = value => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(value)); };
      if (path.pathname === '/config') return json(configuration);
      if (path.pathname === '/state') {
        const runId = path.searchParams.get('runId');
        const session = runId ? runs.get(runId) : path.searchParams.get('sessionId');
        assert.ok(session);
        const ordinal = Number(session.split('_')[1]);
        if (mode === 'waiting65' && ordinal === 65) return json({ runStatus: 'queued', jobStatus: 'waiting', powerState: 'absent', runtimePid: 123 });
        paused.add(session);
        return json({ runStatus: 'completed', jobStatus: 'succeeded', powerState: 'paused',
          environmentId: 'environment_' + ordinal,
          sandboxId: mode === 'changedFirst' && admissions.length === 66 && ordinal === 1 ? 'replacement' : 'sandbox_' + ordinal,
          runtimePid: mode === 'changedRuntime' && ordinal === 65 ? 999 : 123 });
      }
      if (path.pathname === '/finish') {
        let body = ''; request.on('data', part => { body += part; });
        request.on('end', () => { finished = JSON.parse(body); json({ received: true }); }); return;
      }
      if (request.method === 'POST') {
        const requested = path.pathname.split('/')[5];
        assert.equal(request.headers.cookie, 'private-cookie');
        if (admissions.length > 0) assert.ok(paused.has(admissions.at(-1)), 'Previous environment must pause before the next admission');
        const session = requested === 'new' ? 'session_' + (admissions.length + 1) : requested;
        const id = 'run_' + (admissions.length + 1);
        admissions.push(session); runs.set(id, session); request.resume();
        response.writeHead(202, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ sessionId: session, agentRunId: id })); return;
      }
      const id = path.pathname.split('/')[5];
      assert.ok(runs.has(id));
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      for (const [index, type] of ['tool_result', 'agent_run_completed'].entries()) {
        const sequence = index + 1;
        response.write('data: ' + JSON.stringify({ schema: 'session.stream.item.v1', kind: 'committed', agentRunId: id, sourceSequence: sequence,
          event: { schemaVersion: 'session.event.v1', eventVersion: 1, agentRunId: id, sessionId: runs.get(id), type, sequence,
            payload: type === 'tool_result' ? { resultState: 'successWithOutput' } : {} } }) + '\n\n');
      }
      response.end();
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    context.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
    const url = `http://127.0.0.1:${server.address().port}`;
    configuration = { API_BASE: url, PERF_WORKSPACE_ID: 'workspace_fixture', PERF_AGENT_ID: 'agent_fixture', PERF_MODEL_ID: 'model_fixture',
      PERF_COOKIE_HEADER: 'private-cookie', PERF_CSRF_TOKEN: 'private-csrf', PERF_TIMEOUT_MS: '2000',
      PERF_LIFECYCLE_CASE: 'pausedRegistry', PERF_STATE_TIMEOUT_MS: '200', PERF_RUNTIME_PID: 123 };
    const result = await new Promise((resolve, reject) => {
      const child = spawn(binary, ['--log-format', 'raw', 'run', '--quiet', '--no-summary', script], {
        env: { ...process.env, PERF_FIXTURE_URL: url, PERF_LIFECYCLE_CASE: 'pausedRegistry', K6_NO_USAGE_REPORT: 'true',
          NO_PROXY: '*', no_proxy: '*', HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', http_proxy: '', https_proxy: '', all_proxy: '' },
      });
      let output = '';
      child.stdout.on('data', part => { output += part; }); child.stderr.on('data', part => { output += part; });
      const timer = setTimeout(() => { child.kill(); reject(new Error('k6 fixture exceeded 15 seconds')); }, 15000);
      child.once('error', error => { clearTimeout(timer); reject(error); });
      child.once('close', code => { clearTimeout(timer); resolve({ code, output }); });
    });
    assert.ok(finished, result.output);
    assert.ok(!result.output.includes('private-cookie') && !result.output.includes('private-csrf'));
    if (mode === 'completed') {
      assert.equal(result.code, 0, result.output); assert.equal(finished.passed, true);
      assert.deepEqual(admissions, [...Array.from({ length: 65 }, (_, i) => 'session_' + (i + 1)), 'session_1']);
      assert.equal(finished.reports.length, 66);
      assert.equal(finished.registry.distinctEnvironments, 65);
      assert.equal(finished.registry.distinctSandboxes, 65);
      assert.equal(finished.registry.firstRestored, true);
      assert.equal(finished.registry.runtimePid, 123);
      assert.ok(finished.reports.every(row => Number.isFinite(row.pauseWaitMs)));
    } else {
      assert.notEqual(result.code, 0); assert.equal(finished.passed, false);
      assert.equal(admissions.length, mode === 'changedFirst' ? 66 : 65);
      assert.equal(finished.reports.length, admissions.length, 'All accepted Runs must remain in the failure handoff');
      assert.equal(finished.errorCode, { waiting65: 'fixture_state_timeout', changedFirst: 'retained_target_changed',
        changedRuntime: 'runtime_process_changed' }[mode]);
    }
  });
}
