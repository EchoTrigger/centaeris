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
  if (res.status !== 200) throw new Error(`login failed: ${res.status} ${res.body}`);
  // 登录轮换 CSRF token，重新取
  res = http.get(`${API}/api/csrf`, { headers: { Cookie: cookieHeader(store) } });
  absorbCookies(store, res);
  csrf = res.json('csrfToken');
  return { store, csrf };
}

export function firstAgentId(store, csrf, workspaceId) {
  const res = request('GET', `/api/workspaces/${workspaceId}/agents`, store, undefined, csrf);
  if (res.status !== 200) throw new Error(`agents failed: ${res.status}`);
  const agents = res.json('agents');
  if (!agents || !agents.length) throw new Error('no agents seeded');
  return agents[0].id;
}

// 幂等创建 mock provider + 三种 profile 的模型，返回 {providerId, models:{instant,marathon,toolstorm}}
export function ensureProviderAndModels(store, csrf) {
  let res = request('GET', '/api/admin/models', store, undefined, csrf);
  const existing = (res.json('models') || []).filter((m) => m.enabled);
  const models = {};
  for (const name of ['perf-instant', 'perf-marathon', 'perf-toolstorm']) {
    const found = existing.find((m) => m.modelName === name);
    if (found) { models[name] = found.id; continue; }
    let providerId;
    const providersRes = request('GET', '/api/admin/model-providers', store, undefined, csrf);
    const providers = (providersRes.json('providers') || []).filter((p) => p.displayName === 'perf-mock' && !p.archivedAt);
    if (providers.length) {
      providerId = providers[0].id;
    } else {
      res = request('POST', '/api/admin/model-providers', store, {
        displayName: 'perf-mock', api: 'openai-responses', apiBase: MOCK_API_BASE, secret: 'sk-perf-dummy',
      }, csrf);
      if (res.status !== 201 && res.status !== 200) throw new Error(`provider create failed: ${res.status} ${res.body}`);
      providerId = res.json('provider').id;
    }
    res = request('POST', '/api/admin/models', store, {
      providerId, modelName: name, displayName: name,
      contextTokens: 200000, maxOutputTokens: 8192, enabled: true,
    }, csrf);
    if (res.status !== 201 && res.status !== 200) throw new Error(`model create failed: ${res.status} ${res.body}`);
    models[name] = (res.json('model') || res.json()).id;
  }
  return models;
}

export function firstWorkspaceId(store, csrf) {
  const res = request('GET', '/api/workspaces', store, undefined, csrf);
  if (res.status !== 200) throw new Error(`workspaces failed: ${res.status}`);
  const list = res.json('workspaces') || [];
  if (!list.length) throw new Error('no workspaces');
  return list[0].id;
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
  if (res.status !== 202) throw new Error(`message failed: ${res.status} ${res.body}`);
  const body = res.json();
  return { sessionId: body.sessionId, agentRunId: body.agentRunId };
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
  for (const line of response.body.split(/\r?\n/)) {
    if (!line.startsWith('data:')) continue;
    const item = JSON.parse(line.slice(5).trim());
    const event = item.event || item;
    events.push(event);
    if (terminals[event.type]) return { status: terminals[event.type], seconds: (Date.now() - started) / 1000, events };
  }
  throw new Error('terminal stream ended without a terminal event');
}
