// Small synthetic experience tasks. Provider configuration and actual request
// limits belong to the fixture owner; this scenario never reads provider keys.
import { Trend, Rate, Counter } from 'k6/metrics';
import crypto from 'k6/crypto';
import { experienceTask, verifyExperienceRun, verifiedToolTimings } from './experience-contract.js';
import { configuredTarget, login, startRun, waitForTerminalObserved } from './common.js';

const task = __ENV.PERF_EXPERIENCE_TASK || 'text';
const vus = Number(__ENV.PERF_EXPERIENCE_VUS || 1);
const iterations = Number(__ENV.PERF_EXPERIENCE_ITERATIONS || 1);
const sameSession = __ENV.PERF_EXPERIENCE_SAME_SESSION || 'true';
const timeoutSeconds = Number(__ENV.PERF_RUN_TERMINAL_TIMEOUT_SECONDS || 120);
const plannedModelTurns = { text: 1, 'single-tool': 2, 'multi-step': 3 }[task];
if (!['text', 'single-tool', 'multi-step'].includes(task) || ![1, 4, 8].includes(vus) || ![1, 3].includes(iterations)
    || !['true', 'false'].includes(sameSession) || !Number.isInteger(timeoutSeconds)
    || timeoutSeconds < 1 || timeoutSeconds > 120 || vus * iterations * plannedModelTurns > 30) {
  throw new Error('bounded supported experience plan required');
}

const failures = new Rate('experience_sample_failed');
const verified = new Counter('experience_samples_verified');
const admission = new Trend('experience_admission_ms', true);
const streamOpened = new Trend('experience_stream_open_ms', true);
const firstSseItem = new Trend('experience_first_sse_item_ms', true);
// Run submission to first observed nonempty assistant text, including committed fallback.
// This is not model-request TTFT: a tool round may precede the first assistant text.
const firstModelText = new Trend('experience_first_model_text_ms', true);
const toolClientInterval = new Trend('experience_tool_client_call_to_result_ms', true);
const secondToolClientInterval = new Trend('experience_second_tool_client_call_to_result_ms', true);
const firstToolCall = new Trend('experience_first_tool_call_observed_ms', true);
const firstToolResult = new Trend('experience_first_successful_tool_result_ms', true);
const terminal = new Trend('experience_terminal_ms', true);
const verifiedTime = new Trend('experience_verified_ms', true);
let currentSession = 'new', previousFailed = false;

export const options = {
  setupTimeout: '30s',
  scenarios: { experience: { executor: 'per-vu-iterations', vus, iterations,
    maxDuration: `${iterations * (timeoutSeconds + 30) + 30}s`, gracefulStop: '5s', exec: 'default' } },
  thresholds: { experience_sample_failed: ['rate<0.01'], experience_samples_verified: [`count==${vus * iterations}`] },
};

export function setup() { return { ...configuredTarget(task === 'text' ? 'short' : 'tools'), ...login() }; }


function record(phase, data, sampleId, admitted, extra = {}) {
  console.log('PERF_EXPERIENCE ' + JSON.stringify({ schema: 'workspace.perf.experience.v1', phase, task,
    sampleId, operationId: sampleId, workspaceId: data.workspaceId, modelConfigRef: data.modelId,
    sessionId: admitted?.sessionId, agentRunId: admitted?.agentRunId, ...extra }));
}

export default function (data) {
  const sampleId = Array.from(new Uint8Array(crypto.randomBytes(16)), byte => byte.toString(16).padStart(2, '0')).join('');
  if (previousFailed) { failures.add(1); record('notSubmitted', data, sampleId, null, { errorCode: 'previous_sample_failed' }); return; }
  const contract = experienceTask(task, sampleId), started = Date.now();
  let admitted, stage = 'admission';
  try {
    admitted = startRun(data.store, data.csrf, data.workspaceId, data.modelId, contract.prompt,
      { sessionId: sameSession === 'true' ? currentSession : 'new', operationId: sampleId, sampleId });
    currentSession = admitted.sessionId;
    const admissionMs = Date.now() - started; admission.add(admissionMs, { task });
    record('accepted', data, sampleId, admitted, { admissionMs, submittedAtMs: started });
    stage = 'terminal';
    const result = waitForTerminalObserved(data.store, data.csrf, admitted.sessionId, admitted.agentRunId, timeoutSeconds);
    stage = 'contract';
    const proof = verifyExperienceRun(result, contract, admitted.sessionId, admitted.agentRunId);
    const toolTimings = verifiedToolTimings(result, proof.toolCount);
    const times = {}, fields = [
      ['streamOpenedAtMs', 'streamOpenMs', streamOpened], ['firstSseItemObservedAtMs', 'firstSseItemMs', firstSseItem],
      ['firstModelTextObservedAtMs', 'firstModelTextMs', firstModelText],
      ['firstToolCallObservedAtMs', 'firstToolCallObservedMs', firstToolCall],
      ['firstSuccessfulToolResultObservedAtMs', 'firstSuccessfulToolResultMs', firstToolResult],
      ['terminalObservedAtMs', 'terminalMs', terminal],
    ];
    for (const [field, name, metric] of fields) {
      const value = result[field];
      if (value === null && (field === 'streamOpenedAtMs' || (!contract.commands.length && field.includes('Tool')))) { times[name] = null; continue; }
      if (!Number.isFinite(value) || value < started) throw new Error('experience receive time unavailable');
      times[name] = value - started;
      metric.add(times[name], { task, ...(name === 'firstModelTextMs' ? { source: result.firstModelTextSource } : {}) });
    }
    for (const timing of toolTimings) {
      if (timing.clientCallToResultMs !== null) toolClientInterval.add(timing.clientCallToResultMs, { task, toolIndex: String(timing.toolIndex) });
      record('toolTiming', data, sampleId, admitted, timing);
    }
    const secondToolClientCallToResultMs = toolTimings.length === 2 ? toolTimings[1].clientCallToResultMs : null;
    if (secondToolClientCallToResultMs !== null) secondToolClientInterval.add(secondToolClientCallToResultMs, { task });
    const verifiedMs = Date.now() - started;
    verifiedTime.add(verifiedMs, { task }); verified.add(1, { task }); failures.add(0, { task });
    record('verified', data, sampleId, admitted, { ...times, ...proof, admissionMs, verifiedMs,
      firstSseItemSource: result.firstSseItemSource, firstModelTextSource: result.firstModelTextSource,
      firstModelTextTimingKind: 'runStartToFirstObservedAssistantText', secondToolClientCallToResultMs,
      artifactDownloadMs: null, cleanup: 'notObserved' });
  } catch (_) {
    previousFailed = true; failures.add(1, { task }); record('failed', data, sampleId, admitted, { errorCode: 'experience_' + stage + '_failed' });
    throw new Error('experience sample failed at ' + stage);
  }
}
