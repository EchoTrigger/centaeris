// 单Run冒烟（k6 版）：单用户端到端。验证 k6 工具链、认证流、
// 消息→run→工具→终态全链路。通过后其它场景才可信。
import { Trend, Rate } from 'k6/metrics';
import { check } from 'k6';
import { login, configuredTarget, startRun, waitForTerminal, verifyToolRun, artifactExpectation, verifyPublishedArtifact, verifyFixedCommand } from './common.js';

const runFailure = new Rate('run_smoke_failed');
const downloadDuration = new Trend('run_smoke_artifact_download_ms', true);
const verifiedDuration = new Trend('run_smoke_verified_duration_ms', true);
const runDuration = new Trend('run_smoke_duration_ms', true);
// k6 permits local-file reads only during initialization.
const observationText = __ENV.PERF_RUN_FIXTURE_PATH === undefined ? null : open(__ENV.PERF_RUN_FIXTURE_PATH);

export const options = {
  vus: 1,
  iterations: 1,
  thresholds: {
    'run_smoke_duration_ms': ['p(95)<120000'],
    'checks': ['rate>0.999'],
    'run_smoke_failed': ['rate<0.01'],
  },
};

function observationFixture() {
  if (observationText === null) return null;
  const text = observationText;
  if (text.length > 8192) throw new Error('bounded Run observation fixture required');
  const value = JSON.parse(text);
  const identity = item => typeof item === 'string' && /^[A-Za-z0-9_.:-]{1,160}$/.test(item);
  const digest = item => typeof item === 'string' && /^sha256:[a-f0-9]{64}$/.test(item);
  const expected = value?.expectedArtifact;
  if (!value || Array.isArray(value)
      || Object.keys(value).sort().join(',') !== 'agentRunId,expectedArtifact,expectedCommandSha256,schema,sessionId'
      || value.schema !== 'workspace.perf.run.observation.v1'
      || !identity(value.sessionId) || !identity(value.agentRunId) || !digest(value.expectedCommandSha256)
      || !expected || Array.isArray(expected) || Object.keys(expected).sort().join(',') !== 'filename,sha256,sizeBytes'
      || typeof expected.filename !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(expected.filename)
      || !Number.isInteger(expected.sizeBytes) || expected.sizeBytes < 1 || expected.sizeBytes > 32 * 1024 * 1024
      || !digest(expected.sha256)) throw new Error('strict Run observation fixture required');
  return value;
}

export default function () {
  try {

    const fixture = observationFixture();
    const expectedArtifact = fixture ? fixture.expectedArtifact : artifactExpectation();
    const { workspaceId, agentId, modelId } = configuredTarget('short');
    const { store, csrf } = login();

    const started = Date.now();
    const { sessionId, agentRunId } = fixture || startRun(store, csrf, workspaceId, agentId, modelId);
    const result = waitForTerminal(store, csrf, sessionId, agentRunId, 120);
    const milliseconds = Date.now() - started;
    runDuration.add(milliseconds);
    verifyToolRun(result, sessionId, agentRunId);
    if (fixture) verifyFixedCommand(result, fixture.expectedCommandSha256);
    const artifact = verifyPublishedArtifact(store, result, sessionId, agentRunId, expectedArtifact);
    downloadDuration.add(artifact.downloadMs);
    verifiedDuration.add(Date.now() - started);
    const seconds = milliseconds / 1000;

    check(result, {
      'run completed': (r) => r.status === 'completed',
    });
    runFailure.add(0);
    if (fixture) console.log('PERF_RECOVERY_RUN ' + JSON.stringify({schema: 'workspace.perf.run.verified.v1',
      sessionId, agentRunId, artifact}));
    console.log(`Run smoke: run ${result.status} in ${seconds.toFixed(1)}s, events=${result.events.length}`);
  } catch (error) {
    runFailure.add(1);
    check(false, { 'run verified': value => value === true });
    throw error;
  }
}
