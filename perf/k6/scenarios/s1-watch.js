// S1a：SSE 稳态观看并发。
// setup 先创建 MARATHON_RUNS 个长流 run（默认每个约 5 分钟），然后
// 观察者以恒定到达率挂到这些 run 的 SSE 流上，保持 WATCH_HOLD 秒。
// 指标：connect→首事件延迟、事件到达计数、committed 事件的传播滞后
// （lag = 客户端时钟 - 服务端 createdAtMs，同宿主机时钟可直接比）。
//
//   k6 run -e WATCH_RATE=30 -e WATCH_HOLD=60 -e MARATHON_RUNS=4 scenarios/s1-watch.js
import { Trend, Counter } from 'k6/metrics';
import { sleep } from 'k6';
import sse from 'k6/x/sse';
import { login, ensureProviderAndModels, firstWorkspaceId, firstAgentId, startRun, request } from './common.js';

const firstEvent = new Trend('s1_time_to_first_event_ms', true);
const eventLag = new Trend('s1_committed_event_lag_ms', true);
const eventsSeen = new Counter('s1_events_seen');

export const options = {
  scenarios: {
    watchers: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.WATCH_RATE || 20),
      timeUnit: '1m',
      duration: __ENV.DURATION || '2m',
      preAllocatedVUs: Number(__ENV.VUS || 30),
      maxVUs: Number(__ENV.MAX_VUS || 200),
      exec: 'watch',
    },
  },
  thresholds: {
    's1_time_to_first_event_ms': ['p(95)<3000'],
  },
};

export function setup() {
  const { store, csrf } = login();
  const models = ensureProviderAndModels(store, csrf);
  const workspaceId = firstWorkspaceId(store, csrf);
  const agentId = firstAgentId(store, csrf, workspaceId);
  const runs = [];
  const count = Number(__ENV.MARATHON_RUNS || 4);
  for (let i = 0; i < count; i += 1) {
    runs.push(startRun(store, csrf, workspaceId, agentId, models['perf-marathon'], `Marathon run ${i} for watchers.`));
    console.log(`marathon run ${i + 1}/${count}: ${runs[i].agentRunId}`);
  }
  return { store, runs };
}

export function watch(data) {
  const target = data.runs[Math.floor(Math.random() * data.runs.length)];
  let sawFirst = false;
  let received = 0;
  const opened = Date.now();
  sse.open(`${__ENV.API_BASE || 'http://localhost:18000'}/api/sessions/${target.sessionId}/agent-runs/${target.agentRunId}/events`,
    { headers: { Cookie: (function () { const c = data.store; return Object.entries(c).map(([k, v]) => `${k}=${v}`).join('; '); })() } },
    function (connection) {
      connection.on('open', function () {
        if (!sawFirst) {
          sawFirst = true;
          firstEvent.add(Date.now() - opened);
        }
      });
      // phymbert/xk6-sse 的回调名是 'event'（含 data/id 字段）
      connection.on('event', function (event) {
        received += 1;
        eventsSeen.add(1);
        if (!sawFirst) {
          sawFirst = true;
          firstEvent.add(Date.now() - opened);
        }
        try {
          const parsed = JSON.parse(event.data || '{}');
          const inner = parsed.event || parsed.item || {};
          const at = inner.createdAtMs || inner.atMs;
          if (at) eventLag.add(Date.now() - at);
        } catch (_) { /* 非 JSON 帧（心跳等）忽略 */ }
      });
      connection.on('error', function () { /* 断开计入下一次重连 */ });
    });
  sleep(Number(__ENV.WATCH_HOLD || 60));
  // 连接由 sleep 结束后的迭代收尾；received 计入场景计数即可
}
