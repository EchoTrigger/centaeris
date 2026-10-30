import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

function subject(environment = {}, change = () => {}) {
  let clock = 1000;
  const metrics = new Map(), admissions = [], records = [];
  class Metric { constructor(name) { this.name = name; metrics.set(name, []); } add(value) { metrics.get(this.name).push(value); } }
  const context = vm.createContext({ __ENV: environment, Date: { now: () => clock++ },
    Trend: Metric, Rate: Metric, Counter: Metric, console: { log(line) { records.push(JSON.parse(line.slice('PERF_EXPERIENCE '.length))); } },
    crypto: { randomBytes: () => new Uint8Array(16).fill(admissions.length + 1) },
    execution: { vu: { idInTest: 1, iterationInScenario: 0 } },
    configuredTarget: profile => ({ workspaceId: 'workspace', agentId: 'agent', modelId: profile }),
    login: () => ({ store: {}, csrf: 'csrf' }),
    startRun(...args) { admissions.push(args); return { sessionId: 'owned-session', agentRunId: 'run-' + admissions.length }; },
    waitForTerminalObserved(store, csrf, sessionId, agentRunId) {
      const args = admissions.at(-1), contract = context.experienceTask(environment.PERF_EXPERIENCE_TASK || 'text', args[6].operationId);
      const events = [];
      for (const [index, command] of contract.commands.entries()) {
        events.push({ type: 'tool_call', payload: { callId: 'call-' + index, toolName: 'bash', normalizedInput: { command: command.command } } });
        events.push({ type: 'tool_result', payload: { callId: 'call-' + index, toolName: 'bash', resultState: 'successWithOutput', modelContent: command.output } });
      }
      events.push({ type: 'assistant_message', payload: { modelMarkdown: contract.finalText, status: 'done' } });
      events.push({ type: 'agent_run_completed', payload: {} });
      const result = { status: 'completed', events: events.map((event, index) => ({ ...event, sessionId, agentRunId, sequence: index + 1, eventId: 'event-' + index, createdAtMs: 1000000 + index * 100 })),
        streamOpenedAtMs: 1010, firstSseItemObservedAtMs: 1020, firstSseItemSource: 'committed',
        firstModelTextObservedAtMs: 1040, firstModelTextSource: 'committedAssistantMessage',
        firstToolCallObservedAtMs: contract.commands.length ? 1025 : null,
        firstSuccessfulToolResultObservedAtMs: contract.commands.length ? 1030 : null, terminalObservedAtMs: 1050 };
      result.toolTimings = contract.commands.map((command, index) => ({ callId: 'call-' + index,
        toolIndex: index + 1, toolName: 'bash', toolCallCreatedAtMs: result.events[index * 2].createdAtMs,
        toolResultCreatedAtMs: result.events[index * 2 + 1].createdAtMs,
        toolCallObservedAtMs: 1025 + index * 15, toolResultObservedAtMs: index === 0 ? 1030 : 1090,
        clientCallToResultMs: index === 0 ? 5 : 50, resultState: 'successWithOutput',
        serverTimingState: 'observed', clientTimingState: 'observed' }));
      change(result); return result;
    },
  });
  const source = fs.readFileSync(new URL('../k6/scenarios/light-experience.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function iteration(data)').replace(/export /g, '');
  vm.runInContext(source + '\nglobalThis.subjectOptions = options;', context);
  return { context, metrics, admissions, records };
}

test('text completion needs the exact synthetic answer and permits no tools or artifact', () => {
  const s = subject(); s.context.iteration(s.context.setup());
  assert.deepEqual(s.metrics.get('experience_sample_failed'), [0]);
  assert.equal(s.records.at(-1).phase, 'verified');
  assert.equal(s.records.at(-1).firstModelTextSource, 'committedAssistantMessage');
  for (const change of [result => { result.events[0].payload.modelMarkdown = 'unrelated completed answer'; },
    result => { result.events[0].agentRunId = 'foreign'; },
    result => { result.events.splice(0, 0, { ...result.events[0], type: 'tool_call', payload: { callId: 'unexpected' } }); }]) {
    const bad = subject({}, change); assert.throws(() => bad.context.iteration(bad.context.setup()));
    assert.deepEqual(bad.metrics.get('experience_sample_failed'), [1]);
  }
});

test('single-tool and multi-step require their actual ordered commands and successful result markers', () => {
  for (const kind of ['single-tool', 'multi-step']) {
    const s = subject({ PERF_EXPERIENCE_TASK: kind }); s.context.iteration(s.context.setup());
    assert.deepEqual(s.metrics.get('experience_sample_failed'), [0]);
    for (const change of [result => { result.events[0].payload.normalizedInput.command = 'echo fake'; },
      result => { result.events[1].payload.modelContent = 'marker missing'; },
      result => { result.events[1].payload.resultState = 'error'; },
      result => { result.events[1].payload.callId = 'foreign'; }]) {
      const bad = subject({ PERF_EXPERIENCE_TASK: kind }, change);
      assert.throws(() => bad.context.iteration(bad.context.setup()));
      assert.deepEqual(bad.metrics.get('experience_sample_failed'), [1]);
    }
  }
});

test('three iterations reuse the admitted Session with distinct operations, while fresh mode uses new', () => {
  for (const sameSession of ['true', 'false']) {
    const s = subject({ PERF_EXPERIENCE_ITERATIONS: '3', PERF_EXPERIENCE_SAME_SESSION: sameSession });
    const data = s.context.setup(); for (let index = 0; index < 3; index++) s.context.iteration(data);
    assert.deepEqual(s.admissions.map(args => args[6].sessionId), sameSession === 'true' ? ['new', 'owned-session', 'owned-session'] : ['new', 'new', 'new']);
    assert.equal(new Set(s.admissions.map(args => args[6].operationId)).size, 3);
  }
});

test('four concurrent users retain per-VU iteration scheduling and require all four proofs', () => {
  const s = subject({ PERF_EXPERIENCE_VUS: '4' });
  assert.equal(s.context.subjectOptions.scenarios.experience.executor, 'per-vu-iterations');
  assert.equal(s.context.subjectOptions.scenarios.experience.vus, 4);
  assert.equal(s.context.subjectOptions.scenarios.experience.iterations, 1);
  assert.equal(s.context.subjectOptions.thresholds.experience_samples_verified[0], 'count==4');
});

test('unsupported plans fail before admission and failed samples stop the Session chain', () => {
  for (const environment of [{ PERF_EXPERIENCE_TASK: 'arbitrary' }, { PERF_EXPERIENCE_TASK: 'toString' }, { PERF_EXPERIENCE_VUS: '2' }, { PERF_EXPERIENCE_ITERATIONS: '50' },
    { PERF_EXPERIENCE_VUS: '4', PERF_EXPERIENCE_ITERATIONS: '3', PERF_EXPERIENCE_TASK: 'multi-step' }]) assert.throws(() => subject(environment));
  const s = subject({}, result => { result.status = 'failed'; }); const data = s.context.setup();
  assert.throws(() => s.context.iteration(data)); s.context.iteration(data);
  assert.equal(s.admissions.length, 1); assert.equal(s.records.at(-1).phase, 'notSubmitted');
});


test('multi-step emits bounded per-call clocks and an independent second client interval after command validation', () => {
  const s = subject({ PERF_EXPERIENCE_TASK: 'multi-step' }, result => {
    result.firstModelTextObservedAtMs = 2050; result.terminalObservedAtMs = 2100;
  });
  s.context.iteration(s.context.setup());
  const timing = s.records.filter(record => record.phase === 'toolTiming');
  assert.equal(timing.length, 2);
  assert.deepEqual(timing.map(record => record.callId), ['call-0', 'call-1']);
  assert.deepEqual(timing.map(record => record.toolIndex), [1, 2]);
  assert.deepEqual(timing.map(record => record.clientCallToResultMs), [5, 50]);
  assert.deepEqual(timing.map(record => record.toolCallCreatedAtMs), [1000000, 1000200]);
  assert.deepEqual(timing.map(record => record.toolCallObservedAtMs), [1025, 1040]);
  assert.ok(timing.every(record => record.contractState === 'verified'));
  const allowed = ['schema','phase','task','sampleId','operationId','workspaceId','modelConfigRef','sessionId','agentRunId',
    'callId','toolIndex','toolName','toolCallCreatedAtMs','toolResultCreatedAtMs','toolCallObservedAtMs','toolResultObservedAtMs',
    'clientCallToResultMs','resultState','serverTimingState','clientTimingState','contractState'].sort();
  for (const record of timing) assert.deepEqual(Object.keys(record).sort(), allowed);
  const proof = s.records.at(-1);
  assert.equal(proof.secondToolClientCallToResultMs, 50);
  assert.equal(proof.firstSuccessfulToolResultMs, 30);
  assert.equal(proof.firstModelTextMs, 1050);
  assert.equal(proof.firstModelTextTimingKind, 'runStartToFirstObservedAssistantText');
  assert.deepEqual(s.metrics.get('experience_second_tool_client_call_to_result_ms'), [50]);
  assert.deepEqual(s.metrics.get('experience_tool_client_call_to_result_ms'), [5, 50]);
});

test('second-tool command, output, order and timing binding must all validate before any timing success log', () => {
  for (const change of [
    result => { result.events[2].payload.normalizedInput.command = 'printf fake'; },
    result => { result.events[3].payload.modelContent = 'wrong second output'; },
    result => { result.events[3].payload.callId = 'call-0'; },
    result => { result.toolTimings[1].callId = 'call-0'; },
    result => { [result.events[1],result.events[2]] = [result.events[2],result.events[1]]; },
  ]) {
    const s = subject({ PERF_EXPERIENCE_TASK: 'multi-step' }, change);
    assert.throws(() => s.context.iteration(s.context.setup()));
    assert.equal(s.records.filter(record => record.phase === 'toolTiming').length, 0);
    assert.equal(s.metrics.get('experience_second_tool_client_call_to_result_ms')?.length || 0, 0);
  }
});

test('single-tool and text keep second-tool timing null rather than deriving it from final text', () => {
  for (const task of ['text','single-tool']) {
    const s=subject({ PERF_EXPERIENCE_TASK: task }); s.context.iteration(s.context.setup());
    assert.equal(s.records.at(-1).secondToolClientCallToResultMs, null);
    assert.equal(s.metrics.get('experience_second_tool_client_call_to_result_ms').length, 0);
  }
});

test('missing server event time is logged as unobserved without changing client intervals', () => {
  const s=subject({ PERF_EXPERIENCE_TASK: 'single-tool' }, result => {
    delete result.events[0].createdAtMs;
    result.toolTimings[0].toolCallCreatedAtMs=null;
    result.toolTimings[0].serverTimingState='notObserved';
  });
  s.context.iteration(s.context.setup());
  const timing=s.records.find(record => record.phase === 'toolTiming');
  assert.equal(timing.toolCallCreatedAtMs, null);
  assert.equal(timing.serverTimingState, 'notObserved');
  assert.equal(timing.clientCallToResultMs, 5);
});
