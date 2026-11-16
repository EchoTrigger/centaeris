// Fixed local-model A/B workload; actual admission, SSE, tool proofs and release.
import http from 'k6/http';
import crypto from 'k6/crypto';
import execution from 'k6/execution';
import { sleep } from 'k6';
import { Trend, Rate, Counter } from 'k6/metrics';
import { configuredTarget, login, startRunObserved, waitForTerminalObserved } from './common.js';
import { experienceTask, verifyExperienceRun, verifiedToolTimings } from './experience-contract.js';

const plan = JSON.parse(__ENV.PERF_WORKING_SET_PLAN || 'null');
const compare = plan?.mode === 'comparison';
if (!plan || Object.keys(plan).sort().join(',') !== 'iterations,mode,vus'
    || !['comparison', 'saturation'].includes(plan.mode)
    || (compare ? plan.vus !== 1 || plan.iterations !== 120 : ![1, 2, 4, 8].includes(plan.vus) || plan.iterations !== 6)
    || !/^http:\/\/127\.0\.0\.1:[0-9]+$/.test(__ENV.PERF_WORKING_SET_CONTROL || '')
    || (compare && !/^[A-Za-z0-9_.:-]{1,160}$/.test(__ENV.PERF_WORKING_SET_SESSION || ''))) {
  throw new Error('explicit frozen working-set fixture plan required');
}
const control = __ENV.PERF_WORKING_SET_CONTROL;
const failed = new Rate('working_set_failed');
const accepted = new Counter('working_set_accepted');
const verified = new Counter('working_set_verified');
const admission = new Trend('working_set_admission_ms', true);
const terminal = new Trend('working_set_terminal_ms', true);
const firstText = new Trend('working_set_first_text_ms', true);
export const options = {
  scenarios: { working_set: { executor: 'per-vu-iterations', vus: plan.vus, iterations: plan.iterations,
    maxDuration: `${plan.iterations * 250 + 30}s`, gracefulStop: '5s' } },
  thresholds: { working_set_failed: ['rate==0'], working_set_verified: [`count==${plan.vus * plan.iterations}`],
    dropped_iterations: ['count==0'] },
  systemTags: ['status', 'name', 'scenario', 'expected_response'],
};
export function setup() { return { ...configuredTarget('tools'), ...login() }; }
let sessionId = compare ? __ENV.PERF_WORKING_SET_SESSION : 'new', sequence = 0, blocked = false;
function record(phase, sampleId, extra = {}) {
  console.log('PERF_WORKING_SET ' + JSON.stringify({ schema: 'runtime.working_set.client.v1', phase,
    mode: plan.mode, vu: execution.vu.idInTest, iteration: sequence + 1,
    cohort: compare && sequence < 20 ? 'warmup' : compare ? 'measured' : 'saturation', sampleId, ...extra }));
}
export default function (data) {
  const sampleId = crypto.sha256(`working-set:${plan.mode}:${plan.vus}:${execution.vu.idInTest}:${sequence}`, 'hex').slice(0, 32);
  if (blocked) { failed.add(1); record('notSubmitted', sampleId); sequence++; return; }
  let admitted;
  try {
    const permit = http.get(control + '/admission', { timeout: '5s' });
    if (permit.status !== 200 || permit.json('allowed') !== true) throw new Error('admission stopped');
    const contract = experienceTask('single-tool', sampleId);
    admitted = startRunObserved(data.store, data.csrf, data.workspaceId, data.modelId, contract.prompt,
      { sessionId, operationId: sampleId, sampleId });
    if (admitted.httpStatus !== 202) throw new Error('Run not accepted');
    sessionId = admitted.sessionId; accepted.add(1);
    record('accepted', sampleId, admitted);
    const result = waitForTerminalObserved(data.store, data.csrf, sessionId, admitted.agentRunId, 120);
    const proof = verifyExperienceRun(result, contract, sessionId, admitted.agentRunId);
    const timings = verifiedToolTimings(result, proof.toolCount);
    const tag = { cohort: compare && sequence < 20 ? 'warmup' : compare ? 'measured' : 'saturation' };
    admission.add(admitted.admissionMs, tag);
    terminal.add(result.terminalObservedAtMs - admitted.submittedAtMs, tag);
    firstText.add(result.firstModelTextObservedAtMs - admitted.submittedAtMs, tag);
    record('verified', sampleId, { agentRunId: admitted.agentRunId, sessionId, ...proof,
      admissionMs: admitted.admissionMs, terminalMs: result.terminalObservedAtMs - admitted.submittedAtMs,
      firstTextMs: result.firstModelTextObservedAtMs - admitted.submittedAtMs, toolTimings: timings });
    const deadline = Date.now() + 120000;
    while (true) {
      const release = http.get(control + '/release/' + encodeURIComponent(admitted.agentRunId), { timeout: '15s' });
      if (release.status !== 200) throw new Error('release observation failed');
      if (release.json('released')) break;
      if (Date.now() >= deadline) throw new Error('release deadline');
      sleep(0.25);
    }
    if (plan.vus > 1 && sequence + 1 < plan.iterations) {
      while (true) {
        const cohort = http.get(`${control}/barrier/${sequence + 1}/${admitted.agentRunId}`, { timeout: '5s' });
        if (cohort.status !== 200) throw new Error('cohort observation failed');
        if (cohort.json('ready')) break;
        if (Date.now() >= deadline) throw new Error('cohort deadline');
        sleep(0.25);
      }
    }
    record('released', sampleId, { agentRunId: admitted.agentRunId, sessionId });
    verified.add(1); failed.add(0);
  } catch (_) {
    blocked = true; failed.add(1);
    record('failed', sampleId, { agentRunId: admitted?.agentRunId, sessionId });
  }
  sequence++;
}
export function handleSummary(data) { return { stdout: JSON.stringify({ metrics: data.metrics }) + '\n' }; }
