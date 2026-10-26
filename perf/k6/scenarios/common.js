// k6 场景共享助手：手动 cookie 管理（Secure cookie 在 http 上需显式回发）、
// 登录、provider/model 幂等创建、发消息、轮询终态。
import http from 'k6/http';
import crypto from 'k6/crypto';
import { sleep } from 'k6';

export const API = __ENV.API_BASE || 'http://localhost:18000';
export const EMAIL = __ENV.PERF_ADMIN_EMAIL || 'perf-admin@localhost.invalid';
export const PASSWORD = __ENV.PERF_ADMIN_PASSWORD;
export const MOCK_API_BASE = 'https://mock-model:9999/v1';

// 从 Set-Cookie 头提取指定 cookie（k6 会把多条 Set-Cookie 拼接，
// 按名字正则提取是唯一稳妥的方式）
function extractCookie(raw, name) {
  const match = (raw || '').match(new RegExp(`(?:^|[;,\\s])${name}=([^;,\\s]+)`));
  return match ? match[1] : null;
}

export function absorbCookies(cookieStore, response) {
  const raw = response.headers['Set-Cookie'] || response.headers['set-cookie'] || '';
  for (const name of ['csrftoken', 'sessionid']) {
    const value = extractCookie(raw, name);
    if (value) cookieStore[name] = value;
  }
}

export function cookieHeader(cookieStore) {
  return Object.entries(cookieStore).map(([k, v]) => `${k}=${v}`).join('; ');
}

export function request(method, path, cookieStore, payload, csrf, tags) {
  const params = { headers: { Cookie: cookieHeader(cookieStore) }, tags: tags || {} };
  let body;
  if (payload !== undefined) {
    body = JSON.stringify(payload);
    params.headers['Content-Type'] = 'application/json';
  }
  if (csrf) params.headers['X-CSRFToken'] = csrf;
  return http.request(method, API + path, body, params);
}

export function login() {
  const store = {};
  let res = http.get(`${API}/api/csrf`);
  absorbCookies(store, res);
  let csrf = res.json('csrfToken');
  res = http.post(`${API}/api/login`,
    JSON.stringify({ email: EMAIL, password: PASSWORD }),
    { headers: { 'Content-Type': 'application/json', 'X-CSRFToken': csrf, Cookie: cookieHeader(store) } });
  absorbCookies(store, res);
  if (res.status !== 200) throw new Error(`login failed: ${res.status}`);
  // 登录轮换 CSRF token，重新取
  res = http.get(`${API}/api/csrf`, { headers: { Cookie: cookieHeader(store) } });
  absorbCookies(store, res);
  csrf = res.json('csrfToken');
  return { store, csrf };
}

// Fixture setup owns model creation. Load generators only use explicit IDs.
export function configuredTarget(profile) {
  const identity = value => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,160}$/.test(value);
  if (typeof __ENV.API_BASE !== 'string' || !/^https?:\/\/[^/?#\s]+$/.test(__ENV.API_BASE)
      || !identity(__ENV.PERF_WORKSPACE_ID) || !identity(__ENV.PERF_AGENT_ID)) {
    throw new Error('invalid explicit workload target');
  }
  let models;
  try { models = JSON.parse(__ENV.PERF_MODEL_REFS || ''); }
  catch (_) { throw new Error('invalid explicit workload target: model references'); }
  if (!models || Array.isArray(models) || typeof models !== 'object'
      || !['short', 'tools', 'long'].includes(profile)
      || Object.keys(models).some(key => !['short', 'tools', 'long'].includes(key) || !identity(models[key]))
      || !identity(models[profile])) {
    throw new Error('invalid explicit workload target: model references');
  }
  return { workspaceId: __ENV.PERF_WORKSPACE_ID, agentId: __ENV.PERF_AGENT_ID, modelId: models[profile] };
}

// 发消息（sessionId=new 即创建会话并启动 run），返回 {sessionId, agentRunId}
export function startRun(store, csrf, workspaceId, agentId, modelId, text) {
  const operationId = Array.from(new Uint8Array(crypto.randomBytes(16)),
    (byte) => byte.toString(16).padStart(2, '0')).join('');
  const res = request('POST',
    `/api/workspaces/${workspaceId}/sessions/new/messages`, store,
    { operationId, agentId,
      text: text || 'Perf load.', attachmentRefs: [], modelConfigRef: modelId },
    csrf, { type: 'post_message' });
  if (res.status !== 202) throw new Error(`message failed: ${res.status}`);
  const body = res.json();
  return { sessionId: body.sessionId, agentRunId: body.agentRunId };
}

export function committedEvent(item, sessionId, agentRunId) {
  if (!item || item.schema !== 'session.stream.item.v1' || item.agentRunId !== agentRunId) {
    throw new Error('stream event binding invalid');
  }
  if (item.kind === 'live') return null;
  const event = item.event;
  if (item.kind !== 'committed' || !event || event.sessionId !== sessionId
      || event.agentRunId !== agentRunId || event.sequence !== item.sourceSequence
      || !Number.isInteger(event.sequence) || event.sequence <= 0
      || typeof event.eventId !== 'string' || !event.eventId) {
    throw new Error('stream committed binding invalid');
  }
  return event;
}

// The server replays committed events and closes this response at the terminal.
// A request timeout bounds the entire response, including active nonterminal streams.
export function waitForTerminal(store, csrf, sessionId, agentRunId, timeoutSeconds = 120) {
  const started = Date.now();
  const response = http.get(`${API}/api/sessions/${sessionId}/agent-runs/${agentRunId}/events`, {
    headers: { Cookie: cookieHeader(store), Accept: 'text/event-stream' },
    timeout: `${timeoutSeconds}s`, tags: { type: 'terminal_events' },
  });
  if (response.status !== 200) throw new Error(`terminal stream failed: ${response.status}`);
  const events = [];
  const terminals = { agent_run_completed: 'completed', agent_run_failed: 'failed', agent_run_interrupted: 'interrupted' };
  let status;
  for (const line of response.body.split(/\r?\n/)) {
    if (!line.startsWith('data:')) continue;
    const item = JSON.parse(line.slice(5).trim());
    const event = committedEvent(item, sessionId, agentRunId);
    if (!event) continue;
    events.push(event);
    if (terminals[event.type]) status = terminals[event.type];
  }
  if (status) return { status, elapsedMs: Date.now() - started, events };
  throw new Error('terminal stream ended without a terminal event');
}

// Completion and tool success are independent facts.
export function verifyToolRun(result, sessionId, agentRunId) {
  if (!result || result.status !== 'completed' || !Array.isArray(result.events)) {
    throw new Error('Run did not complete');
  }
  const calls = new Map(), results = new Set(), eventIds = new Set();
  let terminalCount = 0, previousSequence = 0, bashCount = 0;
  for (const event of result.events) {
    if (!event || event.sessionId !== sessionId || event.agentRunId !== agentRunId
        || typeof event.eventId !== 'string' || !event.eventId || eventIds.has(event.eventId)
        || !Number.isInteger(event.sequence) || event.sequence <= previousSequence || terminalCount) {
      throw new Error('Run event identity or ordering invalid');
    }
    eventIds.add(event.eventId); previousSequence = event.sequence;
    const payload = event.payload;
    if (event.type === 'tool_call') {
      if (!payload || typeof payload.callId !== 'string' || !payload.callId || calls.has(payload.callId)
          || typeof payload.toolName !== 'string' || !payload.toolName) {
        throw new Error('tool call identity invalid');
      }
      calls.set(payload.callId, payload.toolName);
    } else if (event.type === 'tool_result') {
      if (!payload || !calls.has(payload.callId) || calls.get(payload.callId) !== payload.toolName
          || results.has(payload.callId)) throw new Error('tool call/result mismatch');
      if (!['successWithOutput', 'successNoOutput'].includes(payload.resultState)) {
        throw new Error('tool failed despite Run completion');
      }
      results.add(payload.callId);
      if (payload.toolName === 'bash') bashCount += 1;
    } else if (['agent_run_completed', 'agent_run_failed', 'agent_run_interrupted'].includes(event.type)) {
      if (event.type !== 'agent_run_completed') throw new Error('Run terminal failed');
      terminalCount += 1;
    }
  }
  if (terminalCount !== 1 || !bashCount || calls.size !== results.size) {
    throw new Error('successful paired tool evidence missing');
  }
  return { toolCount: results.size, bashCount };
}

export function artifactExpectation() {
  let expected;
  try { expected = JSON.parse(__ENV.PERF_ARTIFACT_EXPECTATION || ''); }
  catch (_) { throw new Error('explicit artifact expectation required'); }
  if (!expected || Array.isArray(expected)
      || Object.keys(expected).sort().join(',') !== 'filename,sha256,sizeBytes'
      || typeof expected.filename !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(expected.filename)
      || !Number.isInteger(expected.sizeBytes) || expected.sizeBytes < 0 || expected.sizeBytes > 32 * 1024 * 1024
      || typeof expected.sha256 !== 'string' || !/^sha256:[a-f0-9]{64}$/.test(expected.sha256)) {
    throw new Error('explicit artifact expectation invalid');
  }
  return expected;
}

export function verifyPublishedArtifact(store, result, sessionId, agentRunId, expected) {
  verifyToolRun(result, sessionId, agentRunId);
  if (!expected || typeof expected.filename !== 'string' || !Number.isInteger(expected.sizeBytes)
      || typeof expected.sha256 !== 'string') throw new Error('artifact expectation required');
  const publications = result.events.filter(event => event.type === 'artifact_published');
  if (publications.length !== 1) throw new Error('one actual artifact publication required');
  const fact = publications[0].payload;
  const call = result.events.find(event => event.type === 'tool_call' && event.payload.callId === fact.toolCallId);
  if (!call || call.payload.toolName !== 'publish_artifact'
      || call.payload.normalizedInput?.path !== '/mnt/data/' + expected.filename
      || fact.filename !== expected.filename || fact.sizeBytes !== expected.sizeBytes || fact.sha256 !== expected.sha256
      || typeof fact.publicationId !== 'string' || !/^pub_[a-f0-9]{64}$/.test(fact.publicationId)
      || typeof fact.artifactRef !== 'string' || !/^artifact:[A-Za-z0-9_.:-]{1,160}$/.test(fact.artifactRef)) {
    throw new Error('artifact publication binding or metadata invalid');
  }
  const artifactId = fact.artifactRef.slice('artifact:'.length);
  const started = Date.now();
  const response = http.get(API + '/api/artifacts/' + encodeURIComponent(artifactId) + '/download', {
    headers: { Cookie: cookieHeader(store) }, responseType: 'binary', redirects: 0, timeout: '30s',
    tags: { type: 'artifact_download' },
  });
  if (response.status !== 200) throw new Error('artifact download HTTP failure');
  if (response.body?.byteLength !== expected.sizeBytes) throw new Error('artifact download size mismatch');
  const sha256 = 'sha256:' + crypto.sha256(response.body, 'hex');
  if (sha256 !== expected.sha256) throw new Error('artifact download hash mismatch');
  return { artifactRef: fact.artifactRef, publicationId: fact.publicationId, toolCallId: fact.toolCallId,
    sizeBytes: expected.sizeBytes, sha256, downloadMs: Date.now() - started };
}
