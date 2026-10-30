// Run到达负载：并发 AgentRun 编排吞吐（open model）。
// 以恒定到达率创建新 run（instant mock，秒级完成），不挂观察流。
// 目标：找编排层的容量拐点——响应延迟增长、错误率上升、队列积压。
//
//   k6 run -e RATE=60 -e DURATION=3m scenarios/run-arrival-load.js
//   RATE = 每分钟创建多少个 run（arrival rate，不是用户数）
import { Trend, Counter, Rate } from 'k6/metrics';
import { sleep } from 'k6';
import { login, configuredTarget, startRun } from './common.js';

const createLatency = new Trend('run_arrival_admission_latency_ms', true);
const runAccepted = new Counter('run_arrival_runs_accepted');
const createErrors = new Rate('run_arrival_admission_errors');

export const options = {
  scenarios: {
    runArrivals: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 30),
      timeUnit: '1m',
      duration: __ENV.DURATION || '2m',
      preAllocatedVUs: Number(__ENV.VUS || 50),
      maxVUs: Number(__ENV.MAX_VUS || 300),
      exec: 'admitRun',
    },
  },
  thresholds: {
    'run_arrival_admission_errors': ['rate<0.01'],
    'http_req_duration{type:post_message}': ['p(95)<5000'],
  },
  systemTags: ['status', 'type'],
};

export function setup() {
  const { workspaceId, agentId, modelId } = configuredTarget('short');
  const { store, csrf } = login();
  return { store, csrf, workspaceId, agentId, modelId };
}

export function admitRun(data) {
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
