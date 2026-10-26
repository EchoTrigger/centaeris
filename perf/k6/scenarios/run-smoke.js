// 单Run冒烟（k6 版）：单用户端到端。验证 k6 工具链、认证流、
// 消息→run→工具→终态全链路。通过后其它场景才可信。
import { Trend, Rate } from 'k6/metrics';
import { check } from 'k6';
import { login, configuredTarget, startRun, waitForTerminal, verifyToolRun, artifactExpectation, verifyPublishedArtifact } from './common.js';

const runFailure = new Rate('run_smoke_failed');
const downloadDuration = new Trend('run_smoke_artifact_download_ms', true);
const verifiedDuration = new Trend('run_smoke_verified_duration_ms', true);
const runDuration = new Trend('run_smoke_duration_ms', true);

export const options = {
  vus: 1,
  iterations: 1,
  thresholds: {
    'run_smoke_duration_ms': ['p(95)<120000'],
    'checks': ['rate>0.999'],
    'run_smoke_failed': ['rate<0.01'],
  },
};

export default function () {
  try {

    const expectedArtifact = artifactExpectation();
    const { workspaceId, agentId, modelId } = configuredTarget('short');
    const { store, csrf } = login();

    const started = Date.now();
    const { sessionId, agentRunId } = startRun(store, csrf, workspaceId, agentId, modelId);
    const result = waitForTerminal(store, csrf, sessionId, agentRunId, 120);
    const milliseconds = Date.now() - started;
    runDuration.add(milliseconds);
    verifyToolRun(result, sessionId, agentRunId);
    const artifact = verifyPublishedArtifact(store, result, sessionId, agentRunId, expectedArtifact);
    downloadDuration.add(artifact.downloadMs);
    verifiedDuration.add(Date.now() - started);
    const seconds = milliseconds / 1000;

    check(result, {
      'run completed': (r) => r.status === 'completed',
    });
    runFailure.add(0);
    console.log(`Run smoke: run ${result.status} in ${seconds.toFixed(1)}s, events=${result.events.length}`);
  } catch (error) {
    runFailure.add(1);
    check(false, { 'run verified': value => value === true });
    throw error;
  }
}
