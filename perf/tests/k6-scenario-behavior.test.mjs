import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const files = {
  smoke: 'run-smoke.js', arrival: 'run-arrival-load.js', tools: 'tool-execution-load.js',
  stream: 'event-stream-load.js', reconnect: 'event-stream-reconnect.js', soak: 'mixed-workload-soak.js',
};

function scenario(capability, { failed = false } = {}) {
  const metrics = [], admissions = [], waits = [], sleeps = [];
  let clock = 1000;
  class Metric {
    constructor(name) { this.name = name; this.values = []; metrics.push(this); }
    add(value) { this.values.push(value); }
  }
  const context = vm.createContext({
    __ENV: {}, console: { log() {} }, Date: { now() { const now = clock; clock += 20; return now; } },
    Trend: Metric, Counter: Metric, Rate: Metric, check(value, checks) {
      assert.ok(Object.values(checks).every(predicate => predicate(value)));
    },
    login: () => ({ store: { sessionid: 'owned' }, csrf: 'csrf' }),
    ensureProviderAndModels: () => ({ 'perf-instant': 'instant', 'perf-toolstorm': 'tools', 'perf-marathon': 'long' }),
    firstWorkspaceId: () => 'workspace', firstAgentId: () => 'agent', request() {},
    startRun(...args) {
      admissions.push(args);
      if (failed) throw new Error('admission failure');
      return { sessionId: 'session', agentRunId: `run-${admissions.length}` };
    },
    waitForTerminal(...args) { waits.push(args); return { status: failed ? 'failed' : 'completed', events: [] }; },
    sleep(seconds) { sleeps.push(seconds); },
    sse: { open(url, options, configure) {
      const handlers = {};
      configure({ on(kind, callback) { handlers[kind] = callback; } });
      handlers.open?.();
      handlers.event?.({ data: JSON.stringify({ event: { createdAtMs: 990 } }) });
      handlers.error?.();
    } },
  });
  const source = fs.readFileSync(new URL(`../k6/scenarios/${files[capability]}`, import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '')
    .replace('export default function ()', 'function smokeIteration()')
    .replace(/export /g, '');
  vm.runInContext(source + '\nglobalThis.subjectOptions = options;', context);
  return { context, options: context.subjectOptions, metrics, admissions, waits, sleeps };
}

test('smoke admits one Run and waits for its completed terminal with the existing deadline', () => {
  const s = scenario('smoke');
  assert.equal(s.options.vus, 1); assert.equal(s.options.iterations, 1);
  s.context.smokeIteration();
  assert.equal(s.admissions.length, 1); assert.equal(s.waits.length, 1);
  assert.equal(s.waits[0].at(-1), 120);
});

test('arrival load submits independently of completion with unchanged arrival pacing', () => {
  const s = scenario('arrival');
  const config = Object.values(s.options.scenarios)[0];
  assert.equal(config.executor, 'constant-arrival-rate'); assert.equal(config.rate, 30);
  assert.equal(config.timeUnit, '1m'); assert.equal(config.duration, '2m');
  const setup = s.context.setup(); s.context[config.exec](setup);
  assert.equal(s.admissions.length, 1); assert.equal(s.waits.length, 0);
});

test('tool execution load waits for each terminal with unchanged workload pacing', () => {
  const s = scenario('tools');
  const config = Object.values(s.options.scenarios)[0];
  assert.equal(config.executor, 'constant-arrival-rate'); assert.equal(config.rate, 6);
  assert.equal(config.duration, '3m');
  s.context[config.exec](s.context.setup());
  assert.equal(s.admissions.length, 1); assert.equal(s.waits.length, 1);
  assert.equal(s.waits[0].at(-1), 120);
});

test('event stream load measures the current open callback and preserves observation holding', () => {
  const s = scenario('stream');
  const config = Object.values(s.options.scenarios)[0];
  assert.equal(config.rate, 20); assert.equal(config.duration, '2m');
  s.context[config.exec](s.context.setup());
  assert.equal(s.admissions.length, 4);
  assert.equal(s.metrics.find(metric => metric.name === 'event_stream_open_ms').values[0], 20);
  assert.deepEqual(s.sleeps, [60]);
});

test('event stream reconnect preserves target count, open timing and hold duration', () => {
  const s = scenario('reconnect');
  const config = Object.values(s.options.scenarios)[0];
  assert.equal(config.rate, 20); assert.equal(config.duration, '2m');
  s.context[config.exec](s.context.setup());
  assert.equal(s.admissions.length, 3);
  assert.equal(s.metrics.find(metric => metric.name === 'event_stream_reconnect_open_ms').values[0], 20);
  assert.deepEqual(s.sleeps, [5]);
});

test('mixed soak preserves three independent arrival streams and model selections', () => {
  const s = scenario('soak'); const setup = s.context.setup();
  const configs = Object.values(s.options.scenarios);
  assert.deepEqual(configs.map(config => config.rate), [10, 2, 1]);
  assert.deepEqual(configs.map(config => config.timeUnit), ['1m', '1m', '5m']);
  for (const config of configs) { assert.equal(config.duration, '4h'); s.context[config.exec](setup); }
  assert.deepEqual(s.admissions.map(args => args[4]), ['instant', 'tools', 'long']);
  assert.equal(s.waits.length, 0);
});
