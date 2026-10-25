import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('../k6/scenarios/main-path.js', import.meta.url));
const defaultBinary = fileURLToPath(new URL(`../k6/bin/k6${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const binary = process.env.K6_BINARY || defaultBinary;
const secrets = ['fixture-cookie-private', 'fixture-csrf-private', 'fixture-model-output-private'];
function item(type, sequence, payload = {}) {
  return { schema: 'session.stream.item.v1', kind: 'committed', agentRunId: 'agent_run_fixture', sourceSequence: sequence,
    event: { schemaVersion: 'session.event.v1', eventVersion: 1, eventId: `event_${sequence}`,
      sessionId: 'session_fixture', agentRunId: 'agent_run_fixture', sequence, type, createdAtMs: Date.now(), payload } };
}

async function runFixture(context, mode) {
  const timers = new Set();
  let posts = 0;
  let gets = 0;
  const schedule = (fn, ms) => { const timer = setTimeout(fn, ms); timers.add(timer); };
  const server = http.createServer((request, response) => {
    assert.equal(request.headers.cookie, secrets[0]);
    if (request.method === 'POST') {
      posts += 1;
      assert.equal(request.url, '/api/workspaces/workspace_fixture/sessions/session_fixture/messages');
      assert.equal(request.headers['x-csrftoken'], secrets[1]);
      let body = '';
      request.on('data', chunk => { body += chunk; });
      request.on('end', () => {
        const payload = JSON.parse(body);
        assert.equal(payload.modelConfigRef, 'model_fixture');
        assert.equal(payload.agentId, 'agent_fixture');
        assert.equal(payload.text, 'Run one controlled tool.');
        assert.match(payload.operationId, /^[a-f0-9]{32}$/);
        assert.deepEqual(payload.attachmentRefs, []);
        response.writeHead(mode === 'rejected' ? 429 : 202, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify(mode === 'rejected' ? { error: secrets[2] } : {
          sessionId: 'session_fixture', agentRunId: 'agent_run_fixture' }));
      });
      return;
    }
    gets += 1;
    assert.equal(request.url, '/api/sessions/session_fixture/agent-runs/agent_run_fixture/events');
    response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
    response.flushHeaders();
    const send = value => response.write(`data: ${JSON.stringify(value)}\n\n`);
    response.write(': heartbeat\n\n');
    send(item('agent_run_started', 1));
    if (mode === 'timeout') {
      const interval = setInterval(() => response.write(': heartbeat\n\n'), 20);
      response.on('close', () => clearInterval(interval));
      return;
    }
    if (mode === 'missingTool') {
      schedule(() => { send(item('agent_run_completed', 2)); response.end(); }, 40);
      return;
    }
    schedule(() => {
      const tool = item('tool_result', 2, { resultState: mode === 'toolFailed' ? 'error' : 'successWithOutput', text: secrets[2] });
      if (mode === 'foreignRun') tool.agentRunId = 'agent_run_foreign';
      send(tool);
    }, 180);
    schedule(() => {
      send(item(mode === 'failed' ? 'agent_run_failed' : 'agent_run_completed', 3));
      response.end();
    }, 360);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  context.after(async () => {
    for (const timer of timers) clearTimeout(timer);
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  });
  const result = await new Promise((resolve, reject) => {
    const child = spawn(binary, ['--log-format', 'raw', 'run', '--quiet', '--no-summary', script], {
      env: { ...process.env, API_BASE: `http://127.0.0.1:${server.address().port}`, K6_NO_USAGE_REPORT: 'true',
        PERF_COOKIE_HEADER: secrets[0], PERF_CSRF_TOKEN: secrets[1], PERF_WORKSPACE_ID: 'workspace_fixture',
        PERF_AGENT_ID: 'agent_fixture', PERF_MODEL_ID: 'model_fixture', PERF_SESSION_ID: 'session_fixture',
        PERF_MESSAGE: 'Run one controlled tool.', PERF_CASE: mode, PERF_TIMEOUT_MS: mode === 'timeout' ? '180' : '3000',
        NO_PROXY: '*', no_proxy: '*', HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', http_proxy: '', https_proxy: '', all_proxy: '' },
    });
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.stderr.on('data', chunk => { output += chunk; });
    const timer = setTimeout(() => { child.kill(); reject(new Error('Fixture k6 exceeded 15 seconds')); }, 15000);
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', code => { clearTimeout(timer); resolve({ code, output }); });
  });
  for (const secret of secrets) assert.ok(!result.output.includes(secret), 'Credentials and tool output must not enter measurement logs');
  const records = result.output.split(/\r?\n/).filter(line => line.startsWith('MAIN_PATH_RESULT '));
  assert.equal(records.length, 1, result.output);
  return { ...result, report: JSON.parse(records[0].slice('MAIN_PATH_RESULT '.length)), posts, gets };
}

for (const mode of ['completed', 'failed', 'timeout', 'missingTool', 'toolFailed', 'foreignRun', 'rejected']) {
  test(`main-path actual HTTP/SSE measurement: ${mode}`, async context => {
    if (!fs.existsSync(binary)) return context.skip('Set K6_BINARY to the pinned xk6-sse binary');
    const result = await runFixture(context, mode);
    assert.equal(result.posts, 1);
    assert.equal(result.gets, mode === 'rejected' ? 0 : 1);
    assert.equal(result.report.case, mode);
    if (mode === 'completed') {
      assert.equal(result.code, 0, result.output);
      assert.equal(result.report.terminalStatus, 'completed');
      assert.equal(result.report.errorCode, null);
      assert.ok(result.report.acceptedToFirstToolMs >= 160, 'Open/heartbeat/start must not count as first tool');
      assert.ok(result.report.admissionToTerminalMs - result.report.admissionToFirstToolMs >= 150,
        'Tool arrival is timed while streaming, before terminal buffering completes');
      assert.equal(result.report.toolResults, 1);
    } else {
      assert.notEqual(result.code, 0, result.output);
      assert.equal(typeof result.report.errorCode, 'string');
      if (mode === 'timeout' || mode === 'missingTool' || mode === 'foreignRun' || mode === 'rejected') {
        assert.equal(result.report.admissionToFirstToolMs, null);
      }
      if (mode !== 'rejected') assert.equal(result.report.agentRunId, 'agent_run_fixture', 'Accepted work remains identifiable on failure');
      if (mode === 'timeout') {
        assert.equal(result.report.terminalStatus, null);
        assert.ok(result.report.observedMs >= 150 && result.report.observedMs < 1500, 'Active heartbeats cannot extend the overall response timeout');
      }
    }
  });
}
