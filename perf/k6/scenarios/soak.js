// S5：4 小时混合浸泡。三种 profile 并发注入，不设阈值——只采趋势。
//  instant 10/min + toolstorm 2/min + marathon 每 5 分钟 1 个。
// marathon 会占住执行槽 ~5 分钟：负载类资源竞争正是要观察的现象。
//   k6 run scenarios/soak.js
import { sleep } from 'k6';
import { login, ensureProviderAndModels, firstWorkspaceId, firstAgentId, startRun } from './common.js';

const SOAK_HOURS = __ENV.SOAK_HOURS || '4';
const DURATION = `${SOAK_HOURS}h`;

export const options = {
  scenarios: {
    instant: {
      executor: 'constant-arrival-rate', rate: 10, timeUnit: '1m',
      duration: DURATION, preAllocatedVUs: 20, maxVUs: 100, exec: 'instantRun',
    },
    toolstorm: {
      executor: 'constant-arrival-rate', rate: 2, timeUnit: '1m',
      duration: DURATION, preAllocatedVUs: 10, maxVUs: 40, exec: 'stormRun',
    },
    marathon: {
      executor: 'constant-arrival-rate', rate: 1, timeUnit: '5m',
      duration: DURATION, preAllocatedVUs: 3, maxVUs: 8, exec: 'marathonRun',
    },
  },
};

export function setup() {
  const { store, csrf } = login();
  const models = ensureProviderAndModels(store, csrf);
  const workspaceId = firstWorkspaceId(store, csrf);
  const agentId = firstAgentId(store, csrf, workspaceId);
  return { store, csrf, workspaceId, agentId, models };
}

export function instantRun(data) {
  try {
    startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.models['perf-instant']);
  } catch (_) { /* 浸泡期错误由计数与 DB 终态统计呈现 */ }
}

export function stormRun(data) {
  try {
    startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.models['perf-toolstorm']);
  } catch (_) { /* 同上 */ }
}

export function marathonRun(data) {
  try {
    startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.models['perf-marathon']);
  } catch (_) { /* 同上 */ }
}
