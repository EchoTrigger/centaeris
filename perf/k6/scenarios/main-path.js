// One public admission and live SSE observation. The caller owns environment state,
// runs the real Worker, and verifies durable completion/retirement independently.
// Supply credentials through the process environment; never use --http-debug.
import http from 'k6/http';
import crypto from 'k6/crypto';
import sse from 'k6/x/sse';
import { Trend, Rate } from 'k6/metrics';

const timeoutMs = Number(__ENV.PERF_TIMEOUT_MS || 120000);
if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 300000) {
  throw new Error('invalid measurement timeout');
}
const admission = new Trend('main_path_admission_ms', true);
const firstTool = new Trend('main_path_admission_to_first_tool_ms', true);
const acceptedToFirstTool = new Trend('main_path_accepted_to_first_tool_ms', true);
const terminal = new Trend('main_path_admission_to_terminal_ms', true);
const errors = new Rate('main_path_errors');
export const options = {
  scenarios: { mainPath: { executor: 'shared-iterations', vus: 1, iterations: 1,
    maxDuration: `${timeoutMs + 5000}ms`, gracefulStop: '0s' } },
  thresholds: { main_path_errors: ['rate==0'] },
};

const identity = value => typeof value === 'string' && /^[A-Za-z0-9_-]{1,160}$/.test(value);
const terminals = { agent_run_completed: 'completed', agent_run_failed: 'failed', agent_run_interrupted: 'interrupted' };

export function measure(config) {
  const timeoutMs = Number(config.PERF_TIMEOUT_MS || 120000);
  const started = Date.now();
  const report = { schema: 'centaeris.mainPathMeasurement.v1', case: config.PERF_CASE || 'mainPath',
    agentRunId: null, sessionId: null, admissionMs: null, streamOpenMs: null,
    acceptedToFirstToolMs: null, admissionToFirstToolMs: null, admissionToTerminalMs: null,
    terminalStatus: null, toolResults: 0, failedToolResults: 0, errorCode: null, observedMs: null };
  const markError = code => { if (report.errorCode === null) report.errorCode = code; };
  let acceptedAt = null;
  try {
    const workspace = config.PERF_WORKSPACE_ID;
    const agent = config.PERF_AGENT_ID;
    const model = config.PERF_MODEL_ID;
    const session = config.PERF_SESSION_ID || 'new';
    if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 300000 || ![workspace, agent, model, session].every(identity) || !identity(report.case)
        || !config.API_BASE || !config.PERF_COOKIE_HEADER || !config.PERF_CSRF_TOKEN) {
      report.case = 'invalidConfiguration';
      throw new Error('configuration');
    }
    const operationId = Array.from(new Uint8Array(crypto.randomBytes(16)),
      byte => byte.toString(16).padStart(2, '0')).join('');
    const response = http.post(`${config.API_BASE}/api/workspaces/${workspace}/sessions/${session}/messages`,
      JSON.stringify({ operationId, agentId: agent, text: config.PERF_MESSAGE || 'Run one controlled tool.',
        attachmentRefs: [], modelConfigRef: model }), {
        headers: { Cookie: config.PERF_COOKIE_HEADER, 'X-CSRFToken': config.PERF_CSRF_TOKEN, 'Content-Type': 'application/json' },
        timeout: `${timeoutMs}ms`, tags: { type: 'main_path_admission' },
      });
    acceptedAt = Date.now();
    report.admissionMs = acceptedAt - started;
    if (response.status !== 202) {
      markError('admission_rejected_or_unavailable');
    } else {
      const body = response.json();
      if (!identity(body.agentRunId) || !identity(body.sessionId) || (session !== 'new' && body.sessionId !== session)) {
        throw new Error('admission identity');
      }
      report.agentRunId = body.agentRunId;
      report.sessionId = body.sessionId;
      console.log('MAIN_PATH_ACCEPTED ' + JSON.stringify({ case: report.case, agentRunId: report.agentRunId,
        sessionId: report.sessionId, admissionMs: report.admissionMs }));
      const remaining = timeoutMs - (Date.now() - started);
      if (remaining <= 0) {
        markError('measurement_deadline');
      } else {
        const streamed = sse.open(`${config.API_BASE}/api/sessions/${report.sessionId}/agent-runs/${report.agentRunId}/events`, {
          headers: { Cookie: config.PERF_COOKIE_HEADER, Accept: 'text/event-stream' },
          timeout: `${remaining}ms`, tags: { type: 'main_path_events' },
        }, client => {
          client.on('open', () => { report.streamOpenMs = Date.now() - started; });
          client.on('event', frame => {
            if (!frame.data) return; // SSE comments/heartbeats carry no product event.
            try {
              const item = JSON.parse(frame.data);
              if (item.schema !== 'session.stream.item.v1' || item.agentRunId !== report.agentRunId
                  || !['live', 'committed'].includes(item.kind)) throw new Error('envelope');
              if (item.kind === 'live') return;
              const event = item.event;
              if (!event || event.agentRunId !== report.agentRunId || event.sessionId !== report.sessionId
                  || event.schemaVersion !== 'session.event.v1' || event.eventVersion !== 1
                  || event.sequence !== item.sourceSequence) throw new Error('event binding');
              if (event.type === 'tool_result') {
                report.toolResults += 1;
                if (report.admissionToFirstToolMs === null) {
                  const now = Date.now();
                  report.admissionToFirstToolMs = now - started;
                  report.acceptedToFirstToolMs = now - acceptedAt;
                }
                if (!event.payload || !['successWithOutput', 'successNoOutput'].includes(event.payload.resultState)) {
                  report.failedToolResults += 1;
                  markError('tool_result_failed');
                }
              }
              if (terminals[event.type]) {
                report.terminalStatus = terminals[event.type];
                report.admissionToTerminalMs = Date.now() - started;
                client.close();
              }
            } catch (_) {
              markError('invalid_stream_event');
              client.close();
            }
          });
          client.on('error', () => {
            markError('stream_timeout_or_transport_error');
            client.close();
          });
        });
        if (!streamed || streamed.status !== 200) markError('stream_http_unavailable');
      }
      if (report.terminalStatus === null) markError('terminal_not_observed');
      else if (report.terminalStatus !== 'completed') markError('terminal_' + report.terminalStatus);
      if (report.admissionToFirstToolMs === null) markError('first_tool_not_observed');
    }
  } catch (_) {
    markError('measurement_unavailable');
  } finally {
    report.observedMs = Date.now() - started;
    const tags = { case: report.case, outcome: report.errorCode === null ? 'completed' : 'failed' };
    errors.add(report.errorCode === null ? 0 : 1, tags);
    if (report.admissionMs !== null) admission.add(report.admissionMs, tags);
    if (report.admissionToFirstToolMs !== null) {
      firstTool.add(report.admissionToFirstToolMs, tags);
      acceptedToFirstTool.add(report.acceptedToFirstToolMs, tags);
    }
    if (report.admissionToTerminalMs !== null) terminal.add(report.admissionToTerminalMs, tags);
    console.log('MAIN_PATH_RESULT ' + JSON.stringify(report));
  }
  return report;
}

export default function () { return measure(__ENV); }
