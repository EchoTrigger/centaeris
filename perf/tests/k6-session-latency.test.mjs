import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const basePlan = { mode: 'arrival', task: 'workspace', sessionPolicy: 'rolling', vus: 8,
  iterations: 2, rate: 12, durationSeconds: 180, timeoutSeconds: 120 };
function subject(plan = {}, status = 202, broken = false) {
  let clock = 1000;
  const records = [], submissions = [], metrics = new Map();
  class Metric { constructor(name) { this.name = name; metrics.set(name, []); } add(value) { metrics.get(this.name).push(value); } }
  const context = vm.createContext({ __ENV: { PERF_LATENCY_PLAN: JSON.stringify({ ...basePlan, ...plan }), PERF_LATENCY_CONTROL: 'http://127.0.0.1:18084' },
    Date: { now: () => clock++ }, Trend: Metric, Rate: Metric, Counter: Metric,
    crypto: { randomBytes: () => new Uint8Array(16).fill(submissions.length + 1) }, sleep() {},
    console: { log(line) { records.push(JSON.parse(line.slice('PERF_EXPERIENCE '.length))); } },
    http: { get() { return { status: 200, json: field => field === 'allowed' || field === 'released' }; } },
    configuredTarget: profile => ({ workspaceId: 'workspace', modelId: profile }), login: () => ({ store: {}, csrf: 'csrf' }),
    startRunObserved(...args) {
      submissions.push(args);
      return { sessionId: 'owned-session', agentRunId: 'run-' + submissions.length, submittedAtMs: 1000,
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
      return { status: 'completed', events, streamOpenedAtMs: 1010, firstSseItemObservedAtMs: 1020, firstSseItemSource: 'live',
        firstModelTextObservedAtMs: 1050, firstModelTextSource: 'live', terminalObservedAtMs: 1060,
        firstToolCallObservedAtMs: command ? 1030 : null, firstSuccessfulToolResultObservedAtMs: command ? 1040 : null,
        toolTimings: command ? [{ callId: 'call', toolIndex: 1, toolName: 'bash', toolCallCreatedAtMs: 2000,
          toolResultCreatedAtMs: 2001, toolCallObservedAtMs: 1030, toolResultObservedAtMs: 1040,
          clientCallToResultMs: 10, resultState: 'successWithOutput', serverTimingState: 'observed', clientTimingState: 'observed' }] : [] };
    },
  });
  const contract = fs.readFileSync(new URL('../k6/scenarios/experience-contract.js', import.meta.url), 'utf8').replace(/export /g, '');
  const source = fs.readFileSync(new URL('../k6/scenarios/session-latency.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function iteration(data)').replace(/export /g, '');
  vm.runInContext(contract + '\n' + source + '\nglobalThis.subjectOptions = options;', context);
  return { context, records, submissions, metrics };
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
  assert.equal(s.records[0].httpStatus, 409); assert.equal(s.records[0].errorCode, 'session_workspace_preservation_pending');
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
