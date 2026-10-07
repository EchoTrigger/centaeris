// S2：并发 AgentRun 编排吞吐（open model）。
// 以恒定到达率创建新 run（instant mock，秒级完成），不挂观察流。
// 目标：找编排层的容量拐点——响应延迟增长、错误率上升、队列积压。
//
//   k6 run -e RATE=60 -e DURATION=3m scenarios/s2-runs.js
//   RATE = 每分钟创建多少个 run（arrival rate，不是用户数）
import { Trend, Counter, Rate } from 'k6/metrics';
import { sleep } from 'k6';
import { login, ensureProviderAndModels, firstWorkspaceId, firstAgentId, startRun } from './common.js';

const createLatency = new Trend('s2_create_latency_ms', true);
const runAccepted = new Counter('s2_runs_accepted');
const createErrors = new Rate('s2_create_errors');

export const options = {
  scenarios: {
    runs: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 30),
      timeUnit: '1m',
      duration: __ENV.DURATION || '2m',
      preAllocatedVUs: Number(__ENV.VUS || 50),
      maxVUs: Number(__ENV.MAX_VUS || 300),
      exec: 'createRun',
    },
  },
  thresholds: {
    's2_create_errors{scenario:runs}': ['rate<0.01'],
    'http_req_duration{type:post_message}': ['p(95)<5000'],
  },
  systemTags: ['status', 'type'],
};

export function setup() {
  const { store, csrf } = login();
  const models = ensureProviderAndModels(store, csrf);
  const workspaceId = firstWorkspaceId(store, csrf);
  const agentId = firstAgentId(store, csrf, workspaceId);
  return { store, csrf, workspaceId, agentId, modelId: models['perf-instant'] };
}

export function createRun(data) {
  try {
    const started = Date.now();
    const { sessionId, agentRunId } = startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.modelId);
    createLatency.add(Date.now() - started);
    runAccepted.add(1);
    createErrors.add(0);
    if (__ENV.VERBOSE) console.log(`accepted run ${agentRunId} session ${sessionId}`);
  } catch (error) {
    createErrors.add(1);
    if (__ENV.VERBOSE) console.log(`create failed: ${error.message}`);
  }
  // open model 下 arrival 间隔由执行器控制；这里不 sleep
}
