import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

for (const initialStaggerMs of [0, 1250]) test(`stagger ${initialStaggerMs}: four VUs perform three rolling samples each and reuse exactly four sessions`, () => {
  const samples = Array.from({ length: 12 }, (_, index) => ({ sampleId: (index + 1).toString(16).padStart(32, '0'),
    operationId: (index + 101).toString(16).padStart(32, '0'), workUnits: 2,
    expectedArtifact: { filename: 'perf-result.bin', sizeBytes: 9, sha256: 'sha256:' + (index + 1).toString(16).padStart(64, '0') },
    expectedCommandSha256: 'sha256:' + (index + 1).toString(16).padStart(64, '0') }));
  const requests = [], records = [], sleeps = [];
  for (let vu = 1; vu <= 4; vu += 1) {
    let clock = 0;
    const execution = { vu: { idInTest: vu, iterationInScenario: 0 } };
    class Metric { add() {} }
    const context = vm.createContext({ __ENV: { PERF_ENVIRONMENT_COUNT: '4', PERF_RUNS_PER_ENVIRONMENT: '3', PERF_SAMPLES_PATH: 'external-samples.json', PERF_BETWEEN_RUN_MS: '5000', PERF_INITIAL_STAGGER_MS: String(initialStaggerMs) },
      sleep(seconds) { sleeps.push(seconds); clock += seconds * 1000; }, open: () => JSON.stringify(samples), execution, Date: { now: () => clock }, Trend: Metric, Rate: Metric, Counter: Metric,
      console: { log(value) { records.push(JSON.parse(value.slice('PERF_RUN '.length))); } },
      configuredTarget: () => ({ workspaceId: 'workspace', agentId: 'agent', modelId: 'model' }),
      login: () => ({ store: {}, csrf: 'csrf' }),
      startRun(store, csrf, workspaceId, agentId, modelId, text, options) {
        requests.push({ vu, text, admittedAtMs: clock, ...options });
        return { sessionId: `session-${vu}`, agentRunId: 'run-' + options.sampleId, operationId: options.operationId };
      },
      waitForTerminal: () => ({ status: 'completed', events: [], firstSuccessfulToolResultObservedAtMs: clock, firstCommittedEventObservedAtMs: clock }), verifyToolRun: () => ({ toolCount: 2 }),
      verifyFixedCommand() {}, verifyPublishedArtifact: () => ({ downloadMs: 1, artifactRef: 'artifact:owned' }),
    });
    let source = fs.readFileSync(new URL('../k6/scenarios/multi-environment-workload.js', import.meta.url), 'utf8')
      .replace(/^import .*;\r?\n/gm, '').replace('export default function (data)', 'function workloadIteration(data)').replace(/export /g, '');
    vm.runInContext(source + '\nglobalThis.testOptions=options;', context);
    const config = Object.values(context.testOptions.scenarios)[0];
    assert.equal(config.executor, 'per-vu-iterations'); assert.equal(config.vus, 4); assert.equal(config.iterations, 3);
    const data = context.setup();
    for (let iteration = 0; iteration < 3; iteration += 1) { execution.vu.iterationInScenario = iteration; context.workloadIteration(data); }
  }
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
