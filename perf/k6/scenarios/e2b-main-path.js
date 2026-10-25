// Bounded, single-user lifecycle experiment. k6 owns workload order and waits;
// the external fixture owns isolated services, account data and final retirement.
import http from 'k6/http';
import { sleep } from 'k6';
import { Rate } from 'k6/metrics';
import { measure, options as measurementOptions } from './main-path.js';

const fixture = (__ENV.PERF_FIXTURE_URL || '').replace(/\/$/, '');
if (!/^http:\/\/127\.0\.0\.1:[0-9]{1,5}$/.test(fixture)) throw new Error('fixture_must_be_loopback');
const suiteErrors = new Rate('main_path_suite_errors');
export const options = {
  scenarios: { serialLifecycle: { executor: 'shared-iterations', vus: 1, iterations: 1, maxDuration: '5m', gracefulStop: '0s' } },
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
function waitState(query, accept, deadline) {
  const end = Math.min(deadline, Date.now() + 20000);
  while (Date.now() < end) {
    const value = state(query);
    if (['failed', 'cancelled'].includes(value.runStatus) || ['failed', 'dead'].includes(value.jobStatus)) fail('durable_run_failed');
    if (accept(value)) return;
    sleep(0.1);
  }
  fail('fixture_state_timeout');
}

export default function (data) {
  const reports = [];
  const sessions = {};
  const deadline = Date.now() + 270000; // Reserve time for the final fixture handoff.
  let errorCode = null;
  let stage = 'configuration';
  try {
    if (data.errorCode) fail(data.errorCode);
    const config = data.config;
    const paused = role => waitState('sessionId=' + encodeURIComponent(sessions[role]), value => value.powerState === 'paused', deadline);
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
          value => value.runStatus === 'completed' && value.jobStatus === 'succeeded', deadline);
      } finally { report.durableWaitMs = Date.now() - waitStarted; }
    }
    runCase('coldA', 'A');
    for (let i = 1; i <= 3; i += 1) runCase(`warmA${i}`, 'A', () => running('A'));
    for (let i = 1; i <= 3; i += 1) runCase(`pausedFreeA${i}`, 'A', () => paused('A'));
    runCase('coldB', 'B', () => paused('A'));
    for (let i = 1; i <= 6; i += 1) {
      const role = i % 2 === 1 ? 'A' : 'B';
      const other = role === 'A' ? 'B' : 'A';
      runCase(`capacity${role}${i}`, role, () => { paused(role); running(other); });
    }
  } catch (error) {
    errorCode = error && typeof error.code === 'string' ? error.code : 'suite_unavailable';
  } finally {
    const summary = { reports, passed: errorCode === null, errorCode };
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
