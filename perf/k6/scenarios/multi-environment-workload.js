// Correctness/concurrency workload: each VU keeps one Session for rolling Runs.
// Physical overlap is established by the privileged host collector, not by VU count.
import execution from 'k6/execution';
import { sleep } from 'k6';
import { Trend, Rate, Counter } from 'k6/metrics';
import { configuredTarget, login, startRun, waitForTerminalObserved as waitForTerminal, verifyToolRun, verifyFixedCommand, verifyPublishedArtifact } from './common.js';

const environmentCount = Number(__ENV.PERF_ENVIRONMENT_COUNT || 4);
const iterations = Number(__ENV.PERF_RUNS_PER_ENVIRONMENT || 3);
const initialStaggerMs = Number(__ENV.PERF_INITIAL_STAGGER_MS || 0);
if (!Number.isInteger(initialStaggerMs) || initialStaggerMs < 0 || initialStaggerMs > 120000
    || initialStaggerMs * (environmentCount - 1) > 120000) throw new Error('bounded initial workload stagger required');
const betweenRunMs = Number(__ENV.PERF_BETWEEN_RUN_MS || 0);
if (!Number.isInteger(betweenRunMs) || betweenRunMs < 0 || betweenRunMs > 60000) throw new Error('bounded between-Run gap required');
const samples = JSON.parse(open(__ENV.PERF_SAMPLES_PATH));
if (!Number.isInteger(environmentCount) || environmentCount < 1 || environmentCount > 64 || !Number.isInteger(iterations) || iterations < 1 || iterations > 20
    || !Array.isArray(samples) || samples.length !== environmentCount * iterations
    || new Set(samples.map(sample => sample.sampleId)).size !== samples.length
    || new Set(samples.map(sample => sample.operationId)).size !== samples.length) {
  throw new Error('pre-generated distinct rolling workload samples required');
}
const failures = new Rate('multi_environment_sample_failed');
const verified = new Counter('multi_environment_samples_verified');
const admissionTime = new Trend('multi_environment_admission_ms', true);
const terminalTime = new Trend('multi_environment_terminal_ms', true);
const firstToolResultTime = new Trend('multi_environment_first_successful_tool_result_ms', true);
const downloadTime = new Trend('multi_environment_artifact_download_ms', true);
const verifiedTime = new Trend('multi_environment_verified_ms', true);
let currentSession = 'new';
let previousFailed = false;

export const options = {
  scenarios: { environmentWork: { executor: 'per-vu-iterations', vus: environmentCount, iterations,
    maxDuration: '10m', exec: 'default' } },
  thresholds: { multi_environment_sample_failed: ['rate<0.01'] },
};

export function setup() {
  const target = configuredTarget('tools');
  return { ...target, ...login() };
}

function record(phase, sample, data, admitted, extra = {}) {
  console.log('PERF_RUN ' + JSON.stringify({ schema: 'workspace.perf.run.sample.v1', phase,
    sampleId: sample.sampleId, operationId: sample.operationId, workspaceId: data.workspaceId,
    modelConfigRef: data.modelId, sessionId: admitted?.sessionId, agentRunId: admitted?.agentRunId, ...extra }));
}

export default function (data) {
  const index = (execution.vu.idInTest - 1) * iterations + execution.vu.iterationInScenario;
  const sample = samples[index];
  if (!sample) throw new Error('rolling sample identity unavailable');
  if (previousFailed) { failures.add(1); record('notSubmitted', sample, data, null, { errorCode: 'previous_sample_failed' }); return; }
  if (execution.vu.iterationInScenario === 0 && initialStaggerMs > 0 && execution.vu.idInTest > 1) {
    sleep((execution.vu.idInTest - 1) * initialStaggerMs / 1000);
  }
  const started = Date.now();
  let admitted; let stage = 'admission';
  try {
    const workload = { sampleId: sample.sampleId, operationId: sample.operationId, workUnits: sample.workUnits };
    admitted = startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.modelId,
      'PERF_FIXED_WORKLOAD ' + JSON.stringify(workload),
      { sessionId: currentSession, operationId: sample.operationId, sampleId: sample.sampleId });
    currentSession = admitted.sessionId;
    admissionTime.add(Date.now() - started);
    record('accepted', sample, data, admitted);
    stage = 'terminal';
    const result = waitForTerminal(data.store, data.csrf, admitted.sessionId, admitted.agentRunId, 120);
    terminalTime.add(Date.now() - started);
    stage = 'tools';
    const tools = verifyToolRun(result, admitted.sessionId, admitted.agentRunId);
    verifyFixedCommand(result, sample.expectedCommandSha256);
    if (!Number.isFinite(result.firstSuccessfulToolResultObservedAtMs)
        || result.firstSuccessfulToolResultObservedAtMs < started) throw new Error('successful tool receive time missing');
    const firstSuccessfulToolResultMs = result.firstSuccessfulToolResultObservedAtMs - started;
    stage = 'artifact';
    const artifact = verifyPublishedArtifact(data.store, result, admitted.sessionId, admitted.agentRunId, sample.expectedArtifact);
    firstToolResultTime.add(firstSuccessfulToolResultMs);
    downloadTime.add(artifact.downloadMs); verifiedTime.add(Date.now() - started);
    failures.add(0); verified.add(1);
    record('verified', sample, data, admitted, { toolCount: tools.toolCount, artifact,
      elapsedMs: Date.now() - started, firstSuccessfulToolResultMs,
      firstEventObserved: Number.isFinite(result.firstCommittedEventObservedAtMs) });
    if (execution.vu.iterationInScenario < iterations - 1 && betweenRunMs > 0) sleep(betweenRunMs / 1000);
  } catch (_) {
    previousFailed = true; failures.add(1);
    record('failed', sample, data, admitted, { errorCode: 'workload_' + stage + '_failed', firstEventObserved: null });
  }
}
