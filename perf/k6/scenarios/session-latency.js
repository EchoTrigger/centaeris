// One iteration submits and observes one Run, then waits for fixture-owned release.
// Arrival scheduling remains independent of completion; release occupies a VU.
import http from 'k6/http';
import crypto from 'k6/crypto';
import execution from 'k6/execution';
import { sleep } from 'k6';
import { Trend, Counter, Rate } from 'k6/metrics';
import { configuredTarget, login, startRunObserved, waitForTerminalObserved } from './common.js';
import { experienceTask, verifyExperienceRun, verifiedToolTimings } from './experience-contract.js';

const plan = JSON.parse(__ENV.PERF_LATENCY_PLAN || 'null');
const fields = 'durationSeconds,iterations,mode,rate,sessionPolicy,task,timeoutSeconds,vus';
const integer = (n, low, high) => Number.isInteger(n) && n >= low && n <= high;
const continuous = plan?.mode === 'continuous';
if (!plan || Object.keys(plan).sort().join(',') !== fields
    || !['iterations', 'arrival', 'continuous'].includes(plan.mode) || !['pairs', 'rolling', 'fresh'].includes(plan.sessionPolicy)
    || !['text', 'single-tool', 'workspace'].includes(plan.task) || !integer(plan.vus, 1, 64)
    || !integer(plan.timeoutSeconds, 1, 120) || !integer(plan.iterations, 1, 60)
    || !integer(plan.rate, 1, 120) || !integer(plan.durationSeconds, 1, continuous ? 540 : 180)
    || (continuous && (plan.durationSeconds !== 540 || plan.rate !== 18 || plan.vus !== 32
      || plan.sessionPolicy !== 'rolling' || plan.task !== 'workspace'))
    || (plan.mode === 'iterations' ? plan.vus * plan.iterations : Math.ceil(plan.rate * plan.durationSeconds / 60)) > 200
    || !/^http:\/\/127\.0\.0\.1:[0-9]+$/.test(__ENV.PERF_LATENCY_CONTROL || '')) {
  throw new Error('explicit bounded session latency plan and loopback fixture control required');
}
const control = __ENV.PERF_LATENCY_CONTROL;
const attempted = new Counter('latency_submissions');
const accepted = new Counter('latency_accepted');
const rejected = new Counter('latency_rejected');
const skipped = new Counter('latency_not_submitted');
const failed = new Rate('latency_observation_failed');
const verified = new Counter('latency_verified');
const admission = new Trend('latency_admission_ms', true);
const firstText = new Trend('latency_first_text_ms', true);
const firstTool = new Trend('latency_first_tool_result_ms', true);
const terminal = new Trend('latency_terminal_ms', true);
const release = new Trend('latency_release_observed_ms', true);
const planned = plan.mode === 'iterations' ? plan.vus * plan.iterations : Math.ceil(plan.rate * plan.durationSeconds / 60);
export const options = {
  setupTimeout: '30s',
  scenarios: { latency: plan.mode === 'iterations'
    ? { executor: 'per-vu-iterations', vus: plan.vus, iterations: plan.iterations,
      maxDuration: `${plan.iterations * (plan.timeoutSeconds * 2 + 10) + 30}s`, gracefulStop: '5s' }
    : { executor: 'constant-arrival-rate', rate: plan.rate, timeUnit: '1m', duration: `${plan.durationSeconds}s`,
      preAllocatedVUs: plan.vus, maxVUs: plan.vus, gracefulStop: `${continuous ? 120 : plan.timeoutSeconds * 2 + 10}s` } },
  thresholds: { latency_observation_failed: ['rate==0'], dropped_iterations: ['count==0'],
    ...(continuous ? { latency_rejected: ['count==0'], latency_not_submitted: ['count==0'] } : {}) },
  systemTags: ['status', 'name', 'scenario', 'expected_response'],
};
export function setup() {
  return { ...configuredTarget(plan.task === 'text' ? 'short' : 'tools'), ...login() };
}
function identity() { return Array.from(new Uint8Array(crypto.randomBytes(16)), byte => byte.toString(16).padStart(2, '0')).join(''); }
function record(phase, data, sampleId, admitted, extra = {}) {
  const at = extra.submittedAtMs ?? admitted?.submittedAtMs ?? Date.now();
  const start = continuous ? execution.scenario.startTime : null;
  const window = continuous ? Math.floor((at - start) / 180000) + 1 : null;
  console.log('PERF_EXPERIENCE ' + JSON.stringify({ schema: 'workspace.perf.experience.v1', phase,
    task: plan.task, sampleId, operationId: sampleId, workspaceId: data.workspaceId,
    modelConfigRef: data.modelId, sessionId: admitted?.sessionId, agentRunId: admitted?.agentRunId,
    ...(continuous ? { arrivalStartedAtMs: start, submissionsStoppedAtMs: start + 540000,
      arrivalWindow: window >= 1 && window <= 3 ? window : null } : {}), ...extra }));
}
let sessionId = 'new', fixtureId, sequence = 0, blocked = false, scheduleRecorded = false;
export default function (data) {
  const sampleId = identity();
  if (continuous && !scheduleRecorded) {
    record('loadSchedule', data, sampleId, null); scheduleRecorded = true;
  }
  if (continuous && execution.scenario.iterationInTest >= planned) {
    record('arrivalCutoff', data, sampleId, null, { errorCode: 'scheduled_iteration_boundary' }); return;
  }
  const permit = http.get(control + '/admission', { tags: { name: 'fixture_admission' }, timeout: '5s' });
  if (permit.status !== 200) throw new Error('fixture admission observation failed');
  if (blocked || !permit.json('allowed')) {
    skipped.add(1); record('notSubmitted', data, sampleId, null, { errorCode: blocked ? 'session_chain_failed' : 'fixture_budget_stopped' }); return;
  }
  const fresh = sessionId === 'new' || plan.sessionPolicy === 'fresh' || (plan.sessionPolicy === 'pairs' && sequence % 2 === 0);
  const turn = fresh ? 'first' : 'followup';
  if (fresh) { sessionId = 'new'; fixtureId = sampleId; }
  const contract = experienceTask(plan.task, sampleId, { fixtureId, turn });
  let admitted, stage = 'admission';
  try {
    admitted = startRunObserved(data.store, data.csrf, data.workspaceId, data.modelId, contract.prompt,
      { sessionId, operationId: sampleId, sampleId,
        ...(continuous ? { submitBeforeAtMs: execution.scenario.startTime + 540000 } : {}),
        onSubmit(observation) {
          attempted.add(1, { turn });
          record('submitted', data, sampleId, null, { turn, ...observation });
        } });
    if (admitted.submitted === false) {
      skipped.add(1); record('notSubmitted', data, sampleId, null, { turn, errorCode: admitted.errorCode,
        notSubmittedAtMs: admitted.notSubmittedAtMs }); return;
    }
    admission.add(admitted.admissionMs, { turn, status: String(admitted.httpStatus) });
    if (admitted.httpStatus !== 202) {
      rejected.add(1, { status: String(admitted.httpStatus), errorCode: admitted.errorCode });
      record('rejected', data, sampleId, null, { turn, httpStatus: admitted.httpStatus, errorCode: admitted.errorCode,
        submittedAtMs: admitted.submittedAtMs, admissionMs: admitted.admissionMs });
      return;
    }
    accepted.add(1, { turn }); sessionId = admitted.sessionId; sequence++;
    record('accepted', data, sampleId, admitted, { turn, submittedAtMs: admitted.submittedAtMs, admissionMs: admitted.admissionMs });
    stage = 'stream';
    const result = waitForTerminalObserved(data.store, data.csrf, sessionId, admitted.agentRunId, plan.timeoutSeconds);
    stage = 'contract';
    const proof = verifyExperienceRun(result, contract, sessionId, admitted.agentRunId);
    const timings = verifiedToolTimings(result, proof.toolCount);
    const started = admitted.submittedAtMs;
    const firstModelTextMs = result.firstModelTextObservedAtMs - started;
    const terminalMs = result.terminalObservedAtMs - started;
    firstText.add(firstModelTextMs, { task: plan.task, turn }); terminal.add(terminalMs, { task: plan.task, turn });
    const firstSuccessfulToolResultMs = result.firstSuccessfulToolResultObservedAtMs === null ? null : result.firstSuccessfulToolResultObservedAtMs - started;
    if (firstSuccessfulToolResultMs !== null) firstTool.add(firstSuccessfulToolResultMs, { task: plan.task, turn });
    for (const timing of timings) record('toolTiming', data, sampleId, admitted, timing);
    record('verified', data, sampleId, admitted, { turn, ...proof, admissionMs: admitted.admissionMs,
      streamOpenMs: result.streamOpenedAtMs === null ? null : result.streamOpenedAtMs - started,
      firstSseItemMs: result.firstSseItemObservedAtMs - started, firstSseItemSource: result.firstSseItemSource,
      firstModelTextMs, firstModelTextSource: result.firstModelTextSource,
      firstModelTextTimingKind: 'runStartToFirstObservedAssistantText',
      firstToolCallObservedMs: result.firstToolCallObservedAtMs === null ? null : result.firstToolCallObservedAtMs - started,
      firstSuccessfulToolResultMs, terminalMs, verifiedMs: Date.now() - started });
    verified.add(1, { turn });
    stage = 'release';
    const deadline = Date.now() + plan.timeoutSeconds * 1000;
    while (true) {
      const response = http.get(control + '/release/' + encodeURIComponent(admitted.agentRunId),
        { tags: { name: 'fixture_release' }, timeout: '5s' });
      if (response.status !== 200) throw new Error('fixture release observation failed');
      if (response.json('released')) {
        const releaseObservedMs = Date.now() - started;
        release.add(releaseObservedMs, { turn }); record('releaseObserved', data, sampleId, admitted, { turn, releaseObservedMs }); break;
      }
      if (Date.now() >= deadline) throw new Error('environment release deadline');
      sleep(0.25);
    }
    // Fixed-concurrency cohorts start the next turn together after all releases.
    // This pacing is outside submission latency and is never used for arrivals.
    if (plan.mode === 'iterations' && plan.vus > 1 && sequence < plan.iterations) {
      stage = 'barrier';
      const barrierDeadline = Date.now() + plan.timeoutSeconds * 1000;
      while (true) {
        const response = http.get(control + '/barrier/' + sequence + '/' + encodeURIComponent(admitted.agentRunId),
          { tags: { name: 'fixture_barrier' }, timeout: '5s' });
        if (response.status !== 200) throw new Error('fixture cohort observation failed');
        if (response.json('ready')) break;
        if (Date.now() >= barrierDeadline) throw new Error('cohort release deadline');
        sleep(0.25);
      }
    }
    failed.add(0);
  } catch (_) {
    blocked = true; failed.add(1); record('failed', data, sampleId, admitted, { turn, errorCode: 'latency_' + stage + '_failed' });
  }
}
export function handleSummary(data) {
  return { stdout: JSON.stringify({ plannedIterations: planned, metrics: data.metrics }) + '\n' };
}
