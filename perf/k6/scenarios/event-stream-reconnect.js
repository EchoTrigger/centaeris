// 事件流重连：SSE churn/重连。观察者反复连接-短持-断开-重连，
// 连接open与首个实际事件分别计时，另记录连接错误。
// 这打的是连接池获取/释放、redis lane 交接与回放路径。
//
//   k6 run -e CHURN_RATE=20 -e DURATION=2m -e HOLD=5 scenarios/event-stream-reconnect.js
import { Trend, Counter } from 'k6/metrics';
import { sleep } from 'k6';
import sse from 'k6/x/sse';
import { login, configuredTarget, startRun, committedEvent } from './common.js';

const reconnectOpen = new Trend('event_stream_reconnect_open_ms', true);
const firstEvent = new Trend('event_stream_reconnect_first_event_ms', true);
const connErrors = new Counter('event_stream_reconnect_errors');
const eventsSeen = new Counter('event_stream_reconnect_events_seen');

export const options = {
  scenarios: {
    eventReconnects: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.CHURN_RATE || 20),
      timeUnit: '1m',
      duration: __ENV.DURATION || '2m',
      preAllocatedVUs: Number(__ENV.VUS || 20),
      maxVUs: Number(__ENV.MAX_VUS || 60),
      exec: 'reconnectEvents',
    },
  },
  thresholds: {
    'event_stream_reconnect_open_ms': ['p(95)<500'],
  },
};

export function setup() {
  const { workspaceId, agentId, modelId } = configuredTarget('long');
  const { store, csrf } = login();
  const runs = [];
  const count = Number(__ENV.MARATHON_RUNS || 3);
  for (let i = 0; i < count; i += 1) {
    runs.push(startRun(store, csrf, workspaceId, agentId, modelId, `Churn target ${i}.`));
  }
  return { store, runs };
}

export function reconnectEvents(data) {
  const target = data.runs[Math.floor(Math.random() * data.runs.length)];
  const cookie = Object.entries(data.store).map(([k, v]) => `${k}=${v}`).join('; ');
  const opened = Date.now();
  let readyAt = 0;
  let sawEvent = false;

  sse.open(
    `${__ENV.API_BASE || 'http://localhost:18000'}/api/sessions/${target.sessionId}/agent-runs/${target.agentRunId}/events`,
    { headers: { Cookie: cookie } },
    function (connection) {
      connection.on('open', function () {
        if (!readyAt) {
          readyAt = Date.now();
          reconnectOpen.add(readyAt - opened);
        }
      });
      connection.on('event', function (event) {
        committedEvent(JSON.parse(event.data), target.sessionId, target.agentRunId);
        if (!sawEvent) { sawEvent = true; firstEvent.add(Date.now() - opened); }
        eventsSeen.add(1);
      });
      connection.on('error', function () {
        connErrors.add(1);
      });
    }
  );
  sleep(Number(__ENV.HOLD || 5));
}
