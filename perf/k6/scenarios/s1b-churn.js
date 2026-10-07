// S1b：SSE churn/重连。观察者反复连接-短持-断开-重连，
// 测重连后首事件延迟（含回放突发）、连接错误率。
// 这打的是连接池获取/释放、redis lane 交接与回放路径。
//
//   k6 run -e CHURN_RATE=20 -e DURATION=2m -e HOLD=5 scenarios/s1b-churn.js
import { Trend, Counter } from 'k6/metrics';
import { sleep } from 'k6';
import sse from 'k6/x/sse';
import { login, ensureProviderAndModels, firstWorkspaceId, firstAgentId, startRun } from './common.js';

const reconnectTTFE = new Trend('s1b_reconnect_ttfte_ms', true);
const connErrors = new Counter('s1b_connection_errors');
const eventsSeen = new Counter('s1b_events_seen');

export const options = {
  scenarios: {
    churn: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.CHURN_RATE || 20),
      timeUnit: '1m',
      duration: __ENV.DURATION || '2m',
      preAllocatedVUs: Number(__ENV.VUS || 20),
      maxVUs: Number(__ENV.MAX_VUS || 60),
      exec: 'churnConnect',
    },
  },
  thresholds: {
    's1b_reconnect_ttfte_ms': ['p(95)<500'],
  },
};

export function setup() {
  const { store, csrf } = login();
  const models = ensureProviderAndModels(store, csrf);
  const workspaceId = firstWorkspaceId(store, csrf);
  const agentId = firstAgentId(store, csrf, workspaceId);
  const runs = [];
  const count = Number(__ENV.MARATHON_RUNS || 3);
  for (let i = 0; i < count; i += 1) {
    runs.push(startRun(store, csrf, workspaceId, agentId, models['perf-marathon'], `Churn target ${i}.`));
  }
  return { store, runs };
}

export function churnConnect(data) {
  const target = data.runs[Math.floor(Math.random() * data.runs.length)];
  const cookie = Object.entries(data.store).map(([k, v]) => `${k}=${v}`).join('; ');
  const opened = Date.now();
  let firstAt = 0;

  sse.open(
    `${__ENV.API_BASE || 'http://localhost:18000'}/api/sessions/${target.sessionId}/agent-runs/${target.agentRunId}/events`,
    { headers: { Cookie: cookie } },
    function (connection) {
      connection.on('open', function () {
        if (!firstAt) {
          firstAt = Date.now();
          reconnectTTFE.add(firstAt - opened);
        }
      });
      connection.on('event', function (event) {
        if (!firstAt) {
          firstAt = Date.now();
          reconnectTTFE.add(firstAt - opened);
        }
        eventsSeen.add(1);
      });
      connection.on('error', function () {
        connErrors.add(1);
      });
    }
  );
  sleep(Number(__ENV.HOLD || 5));
}
