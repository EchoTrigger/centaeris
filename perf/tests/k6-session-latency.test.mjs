import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const basePlan = { mode: 'arrival', task: 'workspace', sessionPolicy: 'rolling', vus: 8,
  iterations: 2, rate: 12, durationSeconds: 180, timeoutSeconds: 120 };
function subject(plan = {}, status = 202, broken = false) {
  let clock = 1000, lastSubmittedAtMs = 1000;
  const records = [], submissions = [], metrics = new Map();
  class Metric { constructor(name) { this.name = name; metrics.set(name, []); } add(value) { metrics.get(this.name).push(value); } }
  const context = vm.createContext({ __ENV: { PERF_LATENCY_PLAN: JSON.stringify({ ...basePlan, ...plan }), PERF_LATENCY_CONTROL: 'http://127.0.0.1:18084' },
    Date: { now: () => clock++ }, Trend: Metric, Rate: Metric, Counter: Metric,
    execution: { scenario: { startTime: 1000, iterationInTest: 0 } },
    crypto: { randomBytes: () => new Uint8Array(16).fill(submissions.length + 1) }, sleep() {},
    console: { log(line) { records.push(JSON.parse(line.slice('PERF_EXPERIENCE '.length))); } },
    http: { get() { return { status: 200, json: field => ['allowed', 'released', 'ready'].includes(field) }; } },
    configuredTarget: profile => ({ workspaceId: 'workspace', modelId: profile }), login: () => ({ store: {}, csrf: 'csrf' }),
    startRunObserved(...args) {
      const submittedAtMs = clock++;
      if (args[5].submitBeforeAtMs !== undefined && submittedAtMs >= args[5].submitBeforeAtMs) {
        return { submitted: false, errorCode: 'arrival_window_closed', notSubmittedAtMs: submittedAtMs };
      }
      args[5].onSubmit?.({ operationId: args[5].operationId, sessionId: args[5].sessionId, submittedAtMs });
      lastSubmittedAtMs = submittedAtMs;
      submissions.push(args);
      return { sessionId: 'owned-session', agentRunId: 'run-' + submissions.length, submittedAtMs,
        admissionMs: 10, httpStatus: status, errorCode: 'session_workspace_preservation_pending' };
    },
    waitForTerminalObserved(store, csrf, sessionId, agentRunId) {
      if (broken) throw new Error('incomplete stream');
      const entry = submissions.at(-1), sampleId = entry[5].sampleId;
      const command = entry[4].match(/Command 1:\n([\s\S]+)\nAfter checking/)?.[1];
      const finalText = 'PERF_EXPERIENCE_' + sampleId + (command ? '_TOOL_OK' : '_TEXT_OK');
      const events = [
        ...(command ? [{ type: 'tool_call', payload: { callId: 'call', toolName: 'bash', normalizedInput: { command } } },
          { type: 'tool_result', payload: { callId: 'call', toolName: 'bash', modelContent: finalText, resultState: 'successWithOutput' } }] : []),
        { type: 'assistant_message', payload: { status: 'done', modelMarkdown: finalText } },
        { type: 'agent_run_completed', payload: {} },
      ].map((event, i) => ({ ...event, sessionId, agentRunId, sequence: i + 1, eventId: 'event-' + i, createdAtMs: 2000 + i }));
      return { status: 'completed', events, streamOpenedAtMs: lastSubmittedAtMs+10, firstSseItemObservedAtMs: lastSubmittedAtMs+20, firstSseItemSource: 'live',
        firstModelTextObservedAtMs: lastSubmittedAtMs+50, firstModelTextSource: 'live', terminalObservedAtMs: lastSubmittedAtMs+60,
        firstToolCallObservedAtMs: command ? lastSubmittedAtMs+30 : null, firstSuccessfulToolResultObservedAtMs: command ? lastSubmittedAtMs+40 : null,
        toolTimings: command ? [{ callId: 'call', toolIndex: 1, toolName: 'bash', toolCallCreatedAtMs: 2000,
          toolResultCreatedAtMs: 2001, toolCallObservedAtMs: 1030, toolResultObservedAtMs: 1040,
          clientCallToResultMs: 10, resultState: 'successWithOutput', serverTimingState: 'observed', clientTimingState: 'observed' }] : [] };
    },
  });
  const contract = fs.readFileSync(new URL('../k6/scenarios/experience-contract.js', import.meta.url), 'utf8').replace(/export /g, '');
  const source = fs.readFileSync(new URL('../k6/scenarios/session-latency.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function iteration(data)').replace(/export /g, '');
  vm.runInContext(contract + '\n' + source + '\nglobalThis.subjectOptions = options;', context);
  return { context, records, submissions, metrics, setClock(value) { clock = value; } };
}
test('arrival iterations observe complete Runs and release without coupling the arrival rate to completion', () => {
  const s = subject(); const data = s.context.setup(); s.context.iteration(data); s.context.iteration(data);
  const options = s.context.subjectOptions.scenarios.latency;
  assert.equal(options.executor, 'constant-arrival-rate'); assert.equal(options.rate, 12);
  assert.equal(options.preAllocatedVUs, 8); assert.equal(options.maxVUs, 8);
  assert.deepEqual(s.submissions.map(args => args[5].sessionId), ['new', 'owned-session']);
  assert.match(s.submissions[0][4], /dd if=\/dev\/urandom/);
  assert.match(s.submissions[1][4], /sha256sum -c/);
  assert.deepEqual(s.metrics.get('latency_verified'), [1, 1]);
  assert.deepEqual(s.records.filter(r => r.phase === 'accepted').map(r => r.turn), ['first', 'followup']);
  assert.equal(s.records.filter(r => r.phase === 'releaseObserved').length, 2);
});
test('rejection remains a submission without an accepted Run or successful latency', () => {
  const s = subject({}, 409); s.context.iteration(s.context.setup());
  assert.deepEqual(s.metrics.get('latency_submissions'), [1]); assert.deepEqual(s.metrics.get('latency_rejected'), [1]);
  assert.deepEqual(s.metrics.get('latency_accepted'), []); assert.deepEqual(s.metrics.get('latency_first_text_ms'), []);
  const rejection = s.records.find(r => r.phase === 'rejected');
  assert.equal(rejection.httpStatus, 409); assert.equal(rejection.errorCode, 'session_workspace_preservation_pending');
});
test('accepted Runs with broken streams remain failed and stop that Session chain', () => {
  const s = subject({}, 202, true); const data = s.context.setup(); s.context.iteration(data); s.context.iteration(data);
  assert.deepEqual(s.metrics.get('latency_accepted'), [1]); assert.deepEqual(s.metrics.get('latency_observation_failed'), [1]);
  assert.deepEqual(s.metrics.get('latency_verified'), []); assert.deepEqual(s.metrics.get('latency_not_submitted'), [1]);
  assert.equal(s.records.find(r => r.phase === 'failed').agentRunId, 'run-1');
});
test('pair policy returns to a fresh Session and unbounded plans fail before requests', () => {
  const s = subject({ mode: 'iterations', sessionPolicy: 'pairs', vus: 1, iterations: 4 }); const data = s.context.setup();
  for (let i = 0; i < 4; i++) s.context.iteration(data);
  assert.deepEqual(s.submissions.map(args => args[5].sessionId), ['new', 'owned-session', 'new', 'owned-session']);
  for (const bad of [{ mode: 'unknown' }, { rate: 121 }, { vus: 0 }, { durationSeconds: 181 },
    { rate: 120, durationSeconds: 180 }, { unknown: 1 }]) assert.throws(() => subject(bad));
});

test('fixed cohorts withhold the next turn when the other environments have not released', () => {
  const s = subject({ mode: 'iterations', vus: 2, iterations: 2 });
  s.context.http.get = url => ({ status: url.includes('/barrier/') ? 503 : 200, json: () => true });
  const data = s.context.setup(); s.context.iteration(data); s.context.iteration(data);
  assert.equal(s.submissions.length, 1);
  assert.equal(s.records.find(r => r.phase === 'failed').errorCode, 'latency_barrier_failed');
  assert.deepEqual(s.metrics.get('latency_not_submitted'), [1]);
});

const continuousPlan = { mode: 'continuous', durationSeconds: 540, rate: 18, vus: 16 };
test('continuous windows share one arrival executor and preserve the Session across window boundaries', () => {
  const s = subject(continuousPlan); const data = s.context.setup();
  s.context.iteration(data);
  s.setClock(181001); s.context.execution.scenario.iterationInTest = 54; s.context.iteration(data);
  s.setClock(361001); s.context.execution.scenario.iterationInTest = 108; s.context.iteration(data);
  const options = s.context.subjectOptions;
  assert.equal(Object.keys(options.scenarios).length, 1);
  assert.equal(options.scenarios.latency.duration, '540s');
  assert.equal(options.scenarios.latency.gracefulStop, '120s');
  assert.deepEqual(s.submissions.map(args => args[5].sessionId), ['new', 'owned-session', 'owned-session']);
  assert.deepEqual(s.records.filter(r => r.phase === 'accepted').map(r => r.arrivalWindow), [1, 2, 3]);
  assert.ok(s.records.filter(r => r.phase === 'submitted').every(r => r.submissionsStoppedAtMs === 541000));
});
test('continuous stop blocks late POSTs and counts the k6 scheduling boundary separately', () => {
  const s = subject(continuousPlan); const data = s.context.setup();
  s.setClock(541000); s.context.execution.scenario.iterationInTest = 161; s.context.iteration(data);
  assert.equal(s.submissions.length, 0);
  assert.equal(s.records.find(r => r.phase === 'notSubmitted').errorCode, 'arrival_window_closed');
  s.context.execution.scenario.iterationInTest = 162; s.context.iteration(data);
  assert.equal(s.submissions.length, 0);
  assert.equal(s.records.filter(r => r.phase === 'arrivalCutoff').length, 1);
  assert.deepEqual(s.metrics.get('latency_submissions'), []);
  assert.deepEqual(s.metrics.get('latency_not_submitted'), [1]);
});
test('continuous acceptance profile cannot quietly change rate, duration, task, or Session policy', () => {
  for (const change of [{ rate: 19 }, { durationSeconds: 539 }, { task: 'text' }, { sessionPolicy: 'pairs' }, { vus: 32 }]) {
    assert.throws(() => subject({ ...continuousPlan, ...change }));
  }
});
