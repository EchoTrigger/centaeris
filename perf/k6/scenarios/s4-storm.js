// S4：沙箱并发风暴。toolstorm 模型每个 run 连续执行 4 轮 bash（沙箱内），
// 以恒定到达率创建 run，每个迭代同步轮询到终态，测量
// run 端到端时长分布（含容器创建 + 4 次 docker exec + 二次模型调用）。
//
//   k6 run -e RATE=12 -e DURATION=3m scenarios/s4-storm.js
import { Trend, Rate } from 'k6/metrics';
import { login, ensureProviderAndModels, firstWorkspaceId, firstAgentId, startRun, waitForTerminal } from './common.js';

const runDuration = new Trend('s4_run_duration_seconds', true);
const runFailure = new Rate('s4_run_failed');

export const options = {
  scenarios: {
    storm: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 6),
      timeUnit: '1m',
      duration: __ENV.DURATION || '3m',
      preAllocatedVUs: Number(__ENV.VUS || 30),
      maxVUs: Number(__ENV.MAX_VUS || 120),
      exec: 'stormRun',
    },
  },
  thresholds: {
    's4_run_failed{scenario:storm}': ['rate<0.05'],
    's4_run_duration_seconds': ['p(95)<90'],
  },
};

export function setup() {
  const { store, csrf } = login();
  const models = ensureProviderAndModels(store, csrf);
  const workspaceId = firstWorkspaceId(store, csrf);
  const agentId = firstAgentId(store, csrf, workspaceId);
  return { store, csrf, workspaceId, agentId, modelId: models['perf-toolstorm'] };
}

export function stormRun(data) {
  const started = Date.now();
  try {
    const { sessionId, agentRunId } = startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.modelId);
    const result = waitForTerminal(data.store, data.csrf, sessionId, agentRunId, Number(__ENV.RUN_TIMEOUT || 120));
    runDuration.add((Date.now() - started) / 1000);
    if (result.status !== 'completed') runFailure.add(1);
  } catch (error) {
    runFailure.add(1);
    console.log(`storm run failed to start: ${error.message}`);
  }
}
