// B1 冒烟（k6 版）：单用户端到端。验证 k6 工具链、认证流、
// 消息→run→工具→终态全链路。通过后其它场景才可信。
import { Trend } from 'k6/metrics';
import { check } from 'k6';
import { login, ensureProviderAndModels, firstWorkspaceId, firstAgentId, startRun, waitForTerminal } from './common.js';

const runDuration = new Trend('b1_run_duration_seconds', true);

export const options = {
  vus: 1,
  iterations: 1,
  thresholds: {
    'b1_run_duration_seconds': ['p(95)<120'],
    'checks': ['rate>0.999'],
  },
};

export default function () {
  const { store, csrf } = login();
  const models = ensureProviderAndModels(store, csrf);
  const workspaceId = firstWorkspaceId(store, csrf);
  const agentId = firstAgentId(store, csrf, workspaceId);

  const started = Date.now();
  const { sessionId, agentRunId } = startRun(store, csrf, workspaceId, agentId, models['perf-instant']);
  const result = waitForTerminal(store, csrf, sessionId, agentRunId, 120);
  const seconds = (Date.now() - started) / 1000;
  runDuration.add(seconds);

  check(result, {
    'run completed': (r) => r.status === 'completed',
  });
  console.log(`B1 smoke: run ${result.status} in ${seconds.toFixed(1)}s, events=${result.events.length}`);
}
