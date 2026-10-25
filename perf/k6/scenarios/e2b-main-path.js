// Bounded, single-user lifecycle experiment. k6 owns workload order and waits;
// the external fixture owns isolated services, account data and final retirement.
import http from 'k6/http';
import { sleep } from 'k6';
import { Rate } from 'k6/metrics';
import { measure, options as measurementOptions } from './main-path.js';

const fixture = (__ENV.PERF_FIXTURE_URL || '').replace(/\/$/, '');
if (!/^http:\/\/127\.0\.0\.1:[0-9]{1,5}$/.test(fixture)) throw new Error('fixture_must_be_loopback');
const registryCase = __ENV.PERF_LIFECYCLE_CASE === 'pausedRegistry';
const suiteErrors = new Rate('main_path_suite_errors');
export const options = {
  scenarios: { serialLifecycle: { executor: 'shared-iterations', vus: 1, iterations: 1, maxDuration: registryCase ? '20m' : '5m', gracefulStop: '0s' } },
  thresholds: { ...measurementOptions.thresholds, main_path_suite_errors: ['rate==0'] },
};
function fail(code) { throw { code }; }
function get(path) {
  const response = http.get(fixture + path, { timeout: '5s', tags: { type: 'fixture_control' } });
  if (response.status !== 200) fail('fixture_http_unavailable');
  try { return response.json(); } catch (_) { fail('fixture_response_invalid'); }
}
export function setup() {
  try { return { config: get('/config') }; }
  catch (_) { return { errorCode: 'fixture_configuration_unavailable' }; }
}
function state(query) { return get('/state?' + query); }
function waitState(query, accept, deadline, timeout = 20000) {
  const end = Math.min(deadline, Date.now() + timeout);
  while (Date.now() < end) {
    const value = state(query);
    if (['failed', 'cancelled'].includes(value.runStatus) || ['failed', 'dead'].includes(value.jobStatus)) fail('durable_run_failed');
    if (accept(value)) return value;
    sleep(0.1);
  }
  fail('fixture_state_timeout');
}

export default function (data) {
  const reports = [];
  const sessions = {};
  const deadline = Date.now() + (registryCase ? 1170000 : 270000); // Reserve time for the final fixture handoff.
  let registry = null;
  let errorCode = null;
  let stage = 'configuration';
  try {
    if (data.errorCode) fail(data.errorCode);
    const config = data.config;
    if (registryCase !== (config.PERF_LIFECYCLE_CASE === 'pausedRegistry')) fail('lifecycle_case_mismatch');
    const stateTimeout = Number(config.PERF_STATE_TIMEOUT_MS || 20000);
    if (!Number.isInteger(stateTimeout) || stateTimeout < 100 || stateTimeout > 20000) fail('state_timeout_invalid');
    const paused = role => waitState('sessionId=' + encodeURIComponent(sessions[role]), value => value.powerState === 'paused', deadline, stateTimeout);
    const running = role => {
      if (state('sessionId=' + encodeURIComponent(sessions[role])).powerState !== 'running') fail('running_precondition_failed');
    };
    function runCase(name, role, before) {
      stage = name;
      if (Date.now() >= deadline) fail('suite_deadline');
      const preconditionStarted = Date.now();
      if (before) before();
      const preconditionMs = Date.now() - preconditionStarted;
      const remaining = deadline - Date.now();
      if (remaining <= 0) fail('suite_deadline');
      const report = measure({ ...config, PERF_CASE: name, PERF_SESSION_ID: sessions[role] || 'new',
        PERF_TIMEOUT_MS: String(Math.min(Number(config.PERF_TIMEOUT_MS || 120000), remaining)) });
      report.preconditionMs = preconditionMs;
      report.durableWaitMs = null;
      reports.push(report);
      if (report.sessionId) sessions[role] = report.sessionId;
      if (report.errorCode) fail(report.errorCode);
      const waitStarted = Date.now();
      try {
        waitState('runId=' + encodeURIComponent(report.agentRunId),
          value => value.runStatus === 'completed' && value.jobStatus === 'succeeded', deadline, stateTimeout);
      } finally { report.durableWaitMs = Date.now() - waitStarted; }
      return report;
    }
    if (registryCase) {
      const environments = new Set(), sandboxes = new Set();
      const expectedPid = Number(config.PERF_RUNTIME_PID);
      if (!Number.isInteger(expectedPid) || expectedPid <= 0) fail('runtime_identity_missing');
      registry = { distinctEnvironments: 0, distinctSandboxes: 0, firstRestored: false, runtimePid: expectedPid };
      function pausedTarget(role, report) {
        const started = Date.now();
        let value;
        try { value = paused(role); }
        finally { report.pauseWaitMs = Date.now() - started; }
        if (value.runtimePid !== expectedPid) fail('runtime_process_changed');
        if (!value.environmentId || !value.sandboxId) fail('retained_target_missing');
        report.environmentId = value.environmentId;
        report.sandboxId = value.sandboxId;
        return value;
      }
      let first;
      for (let index = 1; index <= 65; index += 1) {
        const role = 'environment' + index;
        const report = runCase('createAndPause' + index, role);
        const target = pausedTarget(role, report);
        if (environments.has(target.environmentId) || sandboxes.has(target.sandboxId)) fail('new_environment_reused');
        environments.add(target.environmentId); sandboxes.add(target.sandboxId);
        registry.distinctEnvironments = environments.size; registry.distinctSandboxes = sandboxes.size;
        if (index === 1) first = target;
      }
      const restored = pausedTarget('environment1', runCase('restoreFirst', 'environment1'));
      if (restored.environmentId !== first.environmentId || restored.sandboxId !== first.sandboxId) fail('retained_target_changed');
      registry.firstRestored = true;
    } else {
    runCase('coldA', 'A');
    for (let i = 1; i <= 3; i += 1) runCase(`warmA${i}`, 'A', () => running('A'));
    for (let i = 1; i <= 3; i += 1) runCase(`pausedFreeA${i}`, 'A', () => paused('A'));
    runCase('coldB', 'B', () => paused('A'));
    for (let i = 1; i <= 6; i += 1) {
      const role = i % 2 === 1 ? 'A' : 'B';
      const other = role === 'A' ? 'B' : 'A';
      runCase(`capacity${role}${i}`, role, () => { paused(role); running(other); });
    }
    }
  } catch (error) {
    errorCode = error && typeof error.code === 'string' ? error.code : 'suite_unavailable';
  } finally {
    const summary = { reports, ...(registry ? { registry } : {}), passed: errorCode === null, errorCode };
    try {
      const response = http.post(fixture + '/finish', JSON.stringify(summary), {
        headers: { 'Content-Type': 'application/json' }, timeout: '5s', tags: { type: 'fixture_finish' },
      });
      if (response.status !== 200) fail('fixture_finish_unavailable');
    } catch (_) {
      if (summary.errorCode === null) summary.errorCode = 'fixture_finish_unavailable';
      summary.passed = false;
    }
    suiteErrors.add(summary.passed ? 0 : 1);
    console.log('MAIN_PATH_SUITE ' + JSON.stringify({ ...summary, stage }));
  }
}
