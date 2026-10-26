import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

function budget(terminal, gap, stagger = 0) {
  return JSON.stringify({ scenarioSeconds: 3 * (terminal + 60 + 30) + Math.ceil((3 * stagger + 2 * gap) / 1000),
    setupSeconds: 180, gracefulStopSeconds: 30 });
}

for (const initialStaggerMs of [0, 1250]) test(`stagger ${initialStaggerMs}: four VUs perform three rolling samples each and reuse exactly four sessions`, () => {
  const samples = Array.from({ length: 12 }, (_, index) => ({ sampleId: (index + 1).toString(16).padStart(32, '0'),
    operationId: (index + 101).toString(16).padStart(32, '0'), workUnits: 2,
    expectedArtifact: { filename: 'perf-result.bin', sizeBytes: 9, sha256: 'sha256:' + (index + 1).toString(16).padStart(64, '0') },
    expectedCommandSha256: 'sha256:' + (index + 1).toString(16).padStart(64, '0') }));
  const requests = [], records = [], sleeps = [], deadlines = [];
  const terminalTimeout = initialStaggerMs ? 300 : 120;
  for (let vu = 1; vu <= 4; vu += 1) {
    let clock = 0;
    const execution = { vu: { idInTest: vu, iterationInScenario: 0 } };
    class Metric { add() {} }
    const context = vm.createContext({ __ENV: { PERF_ENVIRONMENT_COUNT: '4', PERF_RUNS_PER_ENVIRONMENT: '3', PERF_SAMPLES_PATH: 'external-samples.json', PERF_BETWEEN_RUN_MS: '5000', PERF_INITIAL_STAGGER_MS: String(initialStaggerMs), PERF_RUN_TERMINAL_TIMEOUT_SECONDS: String(terminalTimeout), PERF_K6_BUDGET: budget(terminalTimeout, 5000, initialStaggerMs) },
      sleep(seconds) { sleeps.push(seconds); clock += seconds * 1000; }, open: () => JSON.stringify(samples), execution, Date: { now: () => clock }, Trend: Metric, Rate: Metric, Counter: Metric,
      console: { log(value) { records.push(JSON.parse(value.slice('PERF_RUN '.length))); } },
      configuredTarget: () => ({ workspaceId: 'workspace', agentId: 'agent', modelId: 'model' }),
      login: () => ({ store: {}, csrf: 'csrf' }),
      startRun(store, csrf, workspaceId, agentId, modelId, text, options) {
        requests.push({ vu, text, admittedAtMs: clock, ...options });
        return { sessionId: `session-${vu}`, agentRunId: 'run-' + options.sampleId, operationId: options.operationId };
      },
      waitForTerminal: (...args) => { deadlines.push(args[4]); return { status: 'completed', events: [], firstSuccessfulToolResultObservedAtMs: clock, firstCommittedEventObservedAtMs: clock }; }, verifyToolRun: () => ({ toolCount: 2 }),
      verifyFixedCommand() {}, verifyPublishedArtifact: () => ({ downloadMs: 1, artifactRef: 'artifact:owned' }),
    });
    let source = fs.readFileSync(new URL('../k6/scenarios/multi-environment-workload.js', import.meta.url), 'utf8')
      .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function workloadIteration(data)').replace(/export /g, '');
    vm.runInContext(source + '\nglobalThis.testOptions=options;', context);
    const config = Object.values(context.testOptions.scenarios)[0];
    assert.equal(config.executor, 'per-vu-iterations'); assert.equal(config.vus, 4); assert.equal(config.iterations, 3); assert.equal(config.maxDuration, `${JSON.parse(budget(terminalTimeout, 5000, initialStaggerMs)).scenarioSeconds}s`);
    assert.equal(context.testOptions.setupTimeout, '180s'); assert.equal(config.gracefulStop, '30s');
    const data = context.setup();
    for (let iteration = 0; iteration < 3; iteration += 1) { execution.vu.iterationInScenario = iteration; context.workloadIteration(data); }
  }
  assert.deepEqual(deadlines, Array(12).fill(terminalTimeout));
  assert.equal(requests.length, 12); assert.equal(new Set(requests.map(row => row.sampleId)).size, 12);
  for (let vu = 1; vu <= 4; vu += 1) assert.deepEqual(requests.filter(row => row.vu === vu).map(row => row.sessionId), ['new', `session-${vu}`, `session-${vu}`]);
  assert.equal(records.filter(row => row.phase === 'verified').length, 12);
  assert.ok(records.filter(row => row.phase === 'verified').every(row => row.firstSuccessfulToolResultMs === 0 && row.firstEventObserved === true));
  const expectedSleeps = Array.from({ length: 4 }, (_, i) => [...(i && initialStaggerMs ? [i * initialStaggerMs / 1000] : []), 5, 5]).flat();
  assert.deepEqual(sleeps, expectedSleeps, 'one initial stagger plus two between-Run gaps per VU');
  for (let vu = 1; vu <= 4; vu += 1) assert.deepEqual(requests.filter(row => row.vu === vu).map(row => row.admittedAtMs), [0, 5000, 10000].map(value => value + (vu - 1) * initialStaggerMs));
  assert.ok(records.filter(row => row.phase === 'verified').every(row => row.elapsedMs === 0), 'client stagger and idle gaps are outside per-Run latency');
  for (const row of requests) assert.equal(JSON.parse(row.text.slice('PERF_FIXED_WORKLOAD '.length)).sampleId, row.sampleId);
});

test('invalid per-Run stream deadlines reject before any workload admission', () => {
  const source = fs.readFileSync(new URL('../k6/scenarios/multi-environment-workload.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function workloadIteration(data)').replace(/export /g, '');
  for (const value of ['0', '-1', 'NaN', '1.5', '601']) {
    const context=vm.createContext({__ENV:{PERF_RUN_TERMINAL_TIMEOUT_SECONDS:value},startRun(){throw new Error('must not admit');}});
    assert.throws(()=>vm.runInContext(source,context),/terminal timeout/);
  }
});

test('declared terminal budgets and idle gaps fit three real rolling iterations without reducing concurrency', () => {
  const source = fs.readFileSync(new URL('../k6/scenarios/multi-environment-workload.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function workloadIteration(data)').replace(/export /g, '');
  const samples = Array.from({ length: 12 }, (_, index) => ({ sampleId: String(index), operationId: `operation-${index}`, workUnits: 100000 }));
  const starts = [], verified = [];
  for (let vu = 1; vu <= 4; vu++) {
    let clock = 0;
    const execution = { vu: { idInTest: vu, iterationInScenario: 0 } };
    class Metric { add() {} }
    const context = vm.createContext({
      __ENV: { PERF_ENVIRONMENT_COUNT: '4', PERF_RUNS_PER_ENVIRONMENT: '3', PERF_RUN_TERMINAL_TIMEOUT_SECONDS: '300',
        PERF_BETWEEN_RUN_MS: '30000', PERF_INITIAL_STAGGER_MS: '0', PERF_SAMPLES_PATH: 'samples.json', PERF_K6_BUDGET: budget(300, 30000) },
      execution, Date: { now: () => clock }, Trend: Metric, Rate: Metric, Counter: Metric,
      open: () => JSON.stringify(samples), sleep(seconds) { clock += seconds * 1000; },
      configuredTarget: () => ({ workspaceId: 'workspace', agentId: 'agent', modelId: 'model' }), login: () => ({ store: {}, csrf: 'csrf' }),
      console: { log(value) { const record = JSON.parse(value.slice('PERF_RUN '.length)); if (record.phase === 'verified') verified.push(record); } },
      startRun(_store, _csrf, _workspace, _agent, _model, _text, options) {
        starts.push({ vu, at: clock, ...options }); clock += 60000;
        return { sessionId: `session-${vu}`, agentRunId: `run-${options.sampleId}` };
      },
      waitForTerminal(_store, _csrf, _session, _run, seconds) {
        clock += (seconds - 1) * 1000;
        return { firstSuccessfulToolResultObservedAtMs: clock, firstCommittedEventObservedAtMs: clock };
      },
      verifyToolRun: () => ({ toolCount: 2 }), verifyFixedCommand() {},
      verifyPublishedArtifact() { clock += 30000; return { downloadMs: 30000 }; },
    });
    vm.runInContext(source + '\nglobalThis.testOptions = options;', context);
    const scenario = Object.values(context.testOptions.scenarios)[0];
    assert.equal(scenario.vus, 4); assert.equal(scenario.iterations, 3);
    assert.equal(starts.filter(row => row.vu === vu).length, 0, 'setup must not precreate Runs');
    const data = context.setup();
    for (let iteration = 0; iteration < 3; iteration++) {
      execution.vu.iterationInScenario = iteration;
      context.workloadIteration(data);
    }
    const units = { s: 1, m: 60, h: 3600 };
    const match = /^(\d+)(s|m|h)$/.exec(scenario.maxDuration);
    assert.ok(match, 'explicit finite scenario budget');
    assert.ok(Number(match[1]) * units[match[2]] * 1000 >= clock,
      `scenario stopped before the declared rolling workload budget (${clock}ms)`);
  }
  assert.equal(starts.length, 12); assert.equal(verified.length, 12);
  assert.deepEqual(starts.filter(row => row.at === 0).map(row => row.vu), [1, 2, 3, 4]);
  for (let vu = 1; vu <= 4; vu++) assert.deepEqual(starts.filter(row => row.vu === vu).map(row => row.sessionId), ['new', `session-${vu}`, `session-${vu}`]);
});
