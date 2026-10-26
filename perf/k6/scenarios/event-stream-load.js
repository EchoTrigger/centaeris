// 事件流负载：SSE 稳态观看并发。
// setup 先创建 MARATHON_RUNS 个长流 run（默认每个约 5 分钟），然后
// 观察者以恒定到达率挂到这些 run 的 SSE 流上，保持 WATCH_HOLD 秒。
// 指标：连接open与首个实际事件分别计时，另记录事件计数和committed传播滞后
// （lag = 客户端时钟 - 服务端 createdAtMs，同宿主机时钟可直接比）。
//
//   k6 run -e WATCH_RATE=30 -e WATCH_HOLD=60 -e MARATHON_RUNS=4 scenarios/event-stream-load.js
import { Trend, Counter } from 'k6/metrics';
import { sleep } from 'k6';
import sse from 'k6/x/sse';
import { login, configuredTarget, startRun, request, committedEvent } from './common.js';

const connectionReady = new Trend('event_stream_open_ms', true);
const firstEvent = new Trend('event_stream_first_event_ms', true);
const eventLag = new Trend('event_stream_committed_event_lag_ms', true);
const eventsSeen = new Counter('event_stream_events_seen');

export const options = {
  scenarios: {
    watchers: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.WATCH_RATE || 20),
      timeUnit: '1m',
      duration: __ENV.DURATION || '2m',
      preAllocatedVUs: Number(__ENV.VUS || 30),
      maxVUs: Number(__ENV.MAX_VUS || 200),
      exec: 'observeEvents',
    },
  },
  thresholds: {
    'event_stream_open_ms': ['p(95)<3000'],
  },
};

export function setup() {
  const { workspaceId, agentId, modelId } = configuredTarget('long');
  const { store, csrf } = login();
  const runs = [];
  const count = Number(__ENV.MARATHON_RUNS || 4);
  for (let i = 0; i < count; i += 1) {
    runs.push(startRun(store, csrf, workspaceId, agentId, modelId, `Marathon run ${i} for watchers.`));
    console.log(`marathon run ${i + 1}/${count}: ${runs[i].agentRunId}`);
  }
  return { store, runs };
}

export function observeEvents(data) {
  const target = data.runs[Math.floor(Math.random() * data.runs.length)];
  let readyMeasured = false;
  let sawEvent = false;
  const opened = Date.now();
  sse.open(`${__ENV.API_BASE || 'http://localhost:18000'}/api/sessions/${target.sessionId}/agent-runs/${target.agentRunId}/events`,
    { headers: { Cookie: (function () { const c = data.store; return Object.entries(c).map(([k, v]) => `${k}=${v}`).join('; '); })() } },
    function (connection) {
      connection.on('open', function () {
        if (!readyMeasured) {
          readyMeasured = true;
          connectionReady.add(Date.now() - opened);
        }
      });
      // phymbert/xk6-sse 的回调名是 'event'（含 data/id 字段）
      connection.on('event', function (event) {
        const parsed = JSON.parse(event.data);
        const inner = committedEvent(parsed, target.sessionId, target.agentRunId);
        if (!sawEvent) { sawEvent = true; firstEvent.add(Date.now() - opened); }
        eventsSeen.add(1);
        if (inner && Number.isFinite(inner.createdAtMs)) eventLag.add(Date.now() - inner.createdAtMs);
      });
      connection.on('error', function () { /* 断开计入下一次重连 */ });
    });
  sleep(Number(__ENV.WATCH_HOLD || 60));
  // 连接由 sleep 结束后的迭代收尾；received 计入场景计数即可
}
