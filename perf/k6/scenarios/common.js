// k6 场景共享助手：手动 cookie 管理（Secure cookie 在 http 上需显式回发）、
// 登录、provider/model 幂等创建、发消息、轮询终态。
import http from 'k6/http';
import sse from 'k6/x/sse';
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
export function startRun(store, csrf, workspaceId, agentId, modelId, text, options = {}) {
  const operationId = options.operationId === undefined ? Array.from(new Uint8Array(crypto.randomBytes(16)),
    (byte) => byte.toString(16).padStart(2, '0')).join('') : options.operationId;
  const sessionId = options.sessionId === undefined ? 'new' : options.sessionId;
  const validId = value => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,160}$/.test(value);
  if (!validId(operationId) || !validId(sessionId)) throw new Error('explicit Run operation/session invalid');
  const res = request('POST',
    `/api/workspaces/${workspaceId}/sessions/${encodeURIComponent(sessionId)}/messages`, store,
    { operationId, agentId,
      text: text || 'Perf load.', attachmentRefs: [], modelConfigRef: modelId },
    csrf, { type: 'post_message' });
  if (res.status !== 202) throw new Error(`message failed: ${res.status}`);
  const body = res.json();
  if (!validId(body.sessionId) || !validId(body.agentRunId)
      || (sessionId !== 'new' && body.sessionId !== sessionId)) throw new Error('admitted Run binding invalid');
  return { sessionId: body.sessionId, agentRunId: body.agentRunId, operationId, sampleId: options.sampleId };
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

// Receive-time measurements use live callbacks; buffered terminal responses cannot provide them.
export function waitForTerminalObserved(store, csrf, sessionId, agentRunId, timeoutSeconds = 120) {
  const started = Date.now(), events = [], calls = new Map();
  // Server event.createdAtMs and client callback Date.now() belong to separate clocks.
  // Event creation is not a database commit timestamp; never subtract across clocks.
  const toolTimings = [], bashCalls = new Map();
  const eventTime = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
  const terminals = { agent_run_completed: 'completed', agent_run_failed: 'failed', agent_run_interrupted: 'interrupted' };
  let status, invalid = false;
  let firstCommittedEventObservedAtMs = null, firstSuccessfulToolResultObservedAtMs = null;
  let streamOpenedAtMs = null, firstSseItemObservedAtMs = null, firstSseItemSource = null;
  let firstModelTextObservedAtMs = null, firstModelTextSource = null;
  let firstToolCallObservedAtMs = null, terminalObservedAtMs = null;
  let previousSequence = 0;
  const eventIds = new Set();
  const response = sse.open(`${API}/api/sessions/${sessionId}/agent-runs/${agentRunId}/events`, {
    headers: { Cookie: cookieHeader(store), Accept: 'text/event-stream' },
    timeout: `${timeoutSeconds}s`, tags: { type: 'terminal_events' },
  }, connection => {
    connection.on('open', () => { if (streamOpenedAtMs === null) streamOpenedAtMs = Date.now(); });
    connection.on('event', item => {
      try {
        const frame = JSON.parse(item.data);
        const event = committedEvent(frame, sessionId, agentRunId);
        const observed = Date.now();
        if (status) throw new Error('event after terminal');
        if (!event && (!Number.isInteger(frame.afterSequence) || frame.afterSequence < 0
            || !Number.isInteger(frame.revision) || frame.revision < 1
            || typeof frame.turnId !== 'string' || !frame.turnId
            || typeof frame.messageId !== 'string' || !frame.messageId || typeof frame.text !== 'string')) {
          throw new Error('live stream position invalid');
        }
        if (event && (event.sequence <= previousSequence || eventIds.has(event.eventId))) {
          throw new Error('committed stream ordering invalid');
        }
        if (firstSseItemObservedAtMs === null) { firstSseItemObservedAtMs = observed; firstSseItemSource = frame.kind; }
        const text = event?.type === 'assistant_message' ? event.payload?.modelMarkdown : (!event ? frame.text : null);
        if (firstModelTextObservedAtMs === null && typeof text === 'string' && text.trim()) {
          firstModelTextObservedAtMs = observed;
          firstModelTextSource = event ? 'committedAssistantMessage' : 'live';
        }
        if (!event) return;
        previousSequence = event.sequence; eventIds.add(event.eventId);
        if (firstCommittedEventObservedAtMs === null) firstCommittedEventObservedAtMs = observed;
        events.push(event);
        if (event.type === 'tool_call') {
          if (typeof event.payload?.callId !== 'string' || !event.payload.callId
              || typeof event.payload.toolName !== 'string' || !event.payload.toolName || calls.has(event.payload.callId)) {
            throw new Error('tool call identity invalid');
          }
          calls.set(event.payload.callId, event.payload.toolName);
          if (event.payload.toolName === 'bash') {
            const timing = { callId: event.payload.callId, toolIndex: toolTimings.length + 1, toolName: 'bash',
              toolCallCreatedAtMs: eventTime(event.createdAtMs), toolResultCreatedAtMs: null,
              toolCallObservedAtMs: observed, toolResultObservedAtMs: null, clientCallToResultMs: null,
              resultState: 'notObserved', serverTimingState: 'notObserved', clientTimingState: 'notObserved' };
            toolTimings.push(timing); bashCalls.set(timing.callId, timing);
          }
          if (firstToolCallObservedAtMs === null) firstToolCallObservedAtMs = observed;
        }
        if (event.type === 'tool_result') {
          const timing = bashCalls.get(event.payload?.callId);
          if (event.payload?.toolName === 'bash' || timing) {
            if (!timing || event.payload.toolName !== 'bash' || timing.toolResultObservedAtMs !== null) {
              throw new Error('bash timing call/result binding invalid');
            }
            timing.toolResultCreatedAtMs = eventTime(event.createdAtMs);
            timing.toolResultObservedAtMs = observed;
            timing.resultState = event.payload.resultState;
            if (Number.isSafeInteger(observed) && Number.isSafeInteger(timing.toolCallObservedAtMs)
                && observed >= timing.toolCallObservedAtMs) {
              timing.clientCallToResultMs = observed - timing.toolCallObservedAtMs;
              timing.clientTimingState = 'observed';
            }
            if (timing.toolCallCreatedAtMs !== null && timing.toolResultCreatedAtMs !== null
                && timing.toolResultCreatedAtMs >= timing.toolCallCreatedAtMs) timing.serverTimingState = 'observed';
          }
        }
        if (event.type === 'tool_result' && firstSuccessfulToolResultObservedAtMs === null
            && typeof event.payload?.callId === 'string' && calls.get(event.payload.callId) === 'bash'
            && event.payload.toolName === 'bash'
            && ['successWithOutput', 'successNoOutput'].includes(event.payload.resultState)) {
          firstSuccessfulToolResultObservedAtMs = observed;
        }
        if (terminals[event.type]) { status = terminals[event.type]; terminalObservedAtMs = observed; connection.close(); }
      } catch (_) { invalid = true; connection.close(); }
    });
    connection.on('error', () => { if (!status) invalid = true; connection.close(); });
  });
  if (invalid || response?.status !== 200 || !status) throw new Error('Run event stream incomplete or invalid');
  return { status, elapsedMs: Date.now() - started, events,
    firstCommittedEventObservedAtMs, firstSuccessfulToolResultObservedAtMs,
    streamOpenedAtMs, firstSseItemObservedAtMs, firstSseItemSource,
    firstModelTextObservedAtMs, firstModelTextSource, firstToolCallObservedAtMs, terminalObservedAtMs, toolTimings };
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

export function verifyFixedCommand(result, expectedSha256) {
  const calls = result.events.filter(event => event.type === 'tool_call' && event.payload.toolName === 'bash');
  if (typeof expectedSha256 !== 'string' || !/^sha256:[a-f0-9]{64}$/.test(expectedSha256) || calls.length !== 1
      || typeof calls[0].payload.normalizedInput?.command !== 'string'
      || 'sha256:' + crypto.sha256(calls[0].payload.normalizedInput.command, 'hex') !== expectedSha256) {
    throw new Error('fixed workload command digest mismatch');
  }
}
