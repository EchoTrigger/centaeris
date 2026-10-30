import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { committedItems } from './session-stream-fixtures.mjs';

for (const toolFailed of [false, true]) test(`real K6 ${toolFailed ? 'rejects failed tool despite completed' : 'verifies public artifact bytes'} using explicit targets`, async context => {
  const binary = process.env.CENTAERIS_K6_BINARY;
  if (!binary || !fs.existsSync(binary)) return context.skip('Set CENTAERIS_K6_BINARY to the pinned existing binary');
  const bytes = Buffer.from('fixed published workload bytes\n');
  const expected = { filename: 'perf-result.bin', sizeBytes: bytes.length,
    sha256: 'sha256:' + createHash('sha256').update(bytes).digest('hex') };
  const events = [
    ['tool_call', { callId: 'bash-call', toolName: 'bash' }],
    ['tool_result', { callId: 'bash-call', toolName: 'bash', resultState: toolFailed ? 'failed' : 'successWithOutput' }],
    ...toolFailed ? [] : [
      ['tool_call', { callId: 'publish-call', toolName: 'publish_artifact', normalizedInput: { path: '/mnt/data/perf-result.bin' } }],
      ['artifact_published', { publicationId: 'pub_' + 'a'.repeat(64), artifactRef: 'artifact:artifact-owned', toolCallId: 'publish-call', ...expected }],
      ['tool_result', { callId: 'publish-call', toolName: 'publish_artifact', resultState: 'successWithOutput' }],
    ],
    ['agent_run_completed', {}],
  ].map(([type, payload], index) => ({ type, payload, eventId: `event-${index + 1}`, sequence: index + 1,
    agentRunId: 'run-owned', sessionId: 'session-owned' }));
  const stream = committedItems(events).map(item => 'data: ' + JSON.stringify(item) + '\n\n').join('');
  const routes = []; let downloads = 0;
  const server = http.createServer((request, response) => {
    routes.push(request.method + ' ' + request.url);
    if (request.url.endsWith('/events')) {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' }); response.end(stream); return;
    }
    if (request.url === '/api/artifacts/artifact-owned/download') {
      downloads += 1; response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': bytes.length }); response.end(bytes); return;
    }
    let body = '';
    request.on('data', chunk => { body += chunk; });
    request.on('end', () => {
      response.setHeader('Content-Type', 'application/json');
      if (request.url === '/api/csrf') { response.end(JSON.stringify({ csrfToken: 'synthetic-csrf' })); return; }
      if (request.url === '/api/login') { response.setHeader('Set-Cookie', 'sessionid=synthetic; Path=/'); response.end(JSON.stringify({ user: { id: 1 } })); return; }
      if (request.url === '/api/workspaces/workspace-owned/sessions/new/messages') {
        const submitted = JSON.parse(body); assert.equal(submitted.agentId, 'agent-owned'); assert.equal(submitted.modelConfigRef, 'model-owned');
        response.writeHead(202); response.end(JSON.stringify({ agentRunId: 'run-owned', sessionId: 'session-owned' })); return;
      }
      response.writeHead(404); response.end('{}');
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  context.after(() => new Promise(resolve => server.close(resolve)));
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'centaeris-k6-publication-'));
  context.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const script = fileURLToPath(new URL('../k6/scenarios/run-smoke.js', import.meta.url));
  const result = await new Promise((resolve, reject) => {
    const child = spawn(binary, ['run', '--summary-export', path.join(temporary, 'summary.json'), script], {
      env: { ...process.env, API_BASE: `http://127.0.0.1:${server.address().port}`, PERF_ADMIN_PASSWORD: 'synthetic-only',
        PERF_WORKSPACE_ID: 'workspace-owned', PERF_AGENT_ID: 'agent-owned', PERF_MODEL_REFS: JSON.stringify({ short: 'model-owned' }),
        PERF_ARTIFACT_EXPECTATION: JSON.stringify(expected), K6_NO_USAGE_REPORT: 'true' },
    });
    let output = ''; child.stdout.on('data', chunk => { output += chunk; }); child.stderr.on('data', chunk => { output += chunk; });
    const timer = setTimeout(() => { child.kill(); reject(new Error('K6 HTTP fixture exceeded deadline')); }, 20000);
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', code => { clearTimeout(timer); resolve({ code, output }); });
  });
  assert.ok(routes.every(route => !route.includes('/admin/')));
  if (toolFailed) {
    assert.notEqual(result.code, 0, result.output); assert.equal(downloads, 0);
    assert.match(result.output, /tool failed despite Run completion/);
  } else { assert.equal(result.code, 0, result.output); assert.equal(downloads, 1); }
});
