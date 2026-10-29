// Small synthetic experience tasks. Provider configuration and actual request
// limits belong to the fixture owner; this scenario never reads provider keys.
import { Trend, Rate, Counter } from 'k6/metrics';
import crypto from 'k6/crypto';
import { configuredTarget, login, startRun, waitForTerminalObserved } from './common.js';

const task = __ENV.PERF_EXPERIENCE_TASK || 'text';
const vus = Number(__ENV.PERF_EXPERIENCE_VUS || 1);
const iterations = Number(__ENV.PERF_EXPERIENCE_ITERATIONS || 1);
const sameSession = __ENV.PERF_EXPERIENCE_SAME_SESSION || 'true';
const timeoutSeconds = Number(__ENV.PERF_RUN_TERMINAL_TIMEOUT_SECONDS || 120);
const plannedModelTurns = { text: 1, 'single-tool': 2, 'multi-step': 3 }[task];
if (!['text', 'single-tool', 'multi-step'].includes(task) || ![1, 4].includes(vus) || ![1, 3].includes(iterations)
    || !['true', 'false'].includes(sameSession) || !Number.isInteger(timeoutSeconds)
    || timeoutSeconds < 1 || timeoutSeconds > 120 || vus * iterations * plannedModelTurns > 30) {
  throw new Error('bounded supported experience plan required');
}

const failures = new Rate('experience_sample_failed');
const verified = new Counter('experience_samples_verified');
const admission = new Trend('experience_admission_ms', true);
const streamOpened = new Trend('experience_stream_open_ms', true);
const firstSseItem = new Trend('experience_first_sse_item_ms', true);
// Run submission to first observed nonempty assistant text, including committed fallback.
// This is not model-request TTFT: a tool round may precede the first assistant text.
const firstModelText = new Trend('experience_first_model_text_ms', true);
const toolClientInterval = new Trend('experience_tool_client_call_to_result_ms', true);
const secondToolClientInterval = new Trend('experience_second_tool_client_call_to_result_ms', true);
const firstToolCall = new Trend('experience_first_tool_call_observed_ms', true);
const firstToolResult = new Trend('experience_first_successful_tool_result_ms', true);
const terminal = new Trend('experience_terminal_ms', true);
const verifiedTime = new Trend('experience_verified_ms', true);
let currentSession = 'new', previousFailed = false;

export const options = {
  setupTimeout: '30s',
  scenarios: { experience: { executor: 'per-vu-iterations', vus, iterations,
    maxDuration: `${iterations * (timeoutSeconds + 30) + 30}s`, gracefulStop: '5s', exec: 'default' } },
  thresholds: { experience_sample_failed: ['rate<0.01'], experience_samples_verified: [`count==${vus * iterations}`] },
};

export function setup() { return { ...configuredTarget(task === 'text' ? 'short' : 'tools'), ...login() }; }

export function experienceTask(kind, sampleId) {
  if (!/^[a-f0-9]{32}$/.test(sampleId)) throw new Error('synthetic experience identity required');
  const marker = 'PERF_EXPERIENCE_' + sampleId;
  let commands, finalText;
  if (kind === 'text') { commands = []; finalText = marker + '_TEXT_OK'; }
  else if (kind === 'single-tool') {
    finalText = marker + '_TOOL_OK';
    commands = [{ command: `printf '${finalText}\\n'`, output: finalText }];
  } else if (kind === 'multi-step') {
    const path = '/mnt/data/perf-experience-' + sampleId + '.txt';
    finalText = marker + '_MULTI_OK:42';
    commands = [
      { command: `printf '21\\n' > '${path}' && printf '${marker}_CREATED\\n'`, output: marker + '_CREATED' },
      { command: `python3 -c 'from pathlib import Path; p=Path("${path}"); print("${marker}_MULTI_OK:"+str(int(p.read_text())*2)); p.unlink()'`, output: finalText },
    ];
  } else throw new Error('unsupported experience task');
  const prompt = commands.length
    ? `Synthetic performance task. Invoke bash exactly ${commands.length} time(s), sequentially, with the following exact command(s). `
      + 'Do not combine commands, add other tool calls, or publish artifacts. Use timeout_ms=10000 for each bash call. '
      + commands.map((value, index) => `Command ${index + 1}:\n${value.command}`).join('\n')
      + `\nAfter checking the tool outputs, reply with exactly ${finalText}`
    : `Synthetic performance task. Do not call any tools. Reply with exactly ${finalText}`;
  return { prompt, commands, finalText };
}

export function verifyExperienceRun(result, contract, sessionId, agentRunId) {
  if (result?.status !== 'completed' || !Array.isArray(result.events)) throw new Error('experience Run did not complete');
  const calls = new Map(), eventIds = new Set();
  let sequence = 0, completed = false, nextCall = 0, nextResult = 0, finalText = null;
  for (const event of result.events) {
    if (!event || event.sessionId !== sessionId || event.agentRunId !== agentRunId
        || !Number.isInteger(event.sequence) || event.sequence <= sequence || completed
        || typeof event.eventId !== 'string' || !event.eventId || eventIds.has(event.eventId)) throw new Error('experience event binding invalid');
    sequence = event.sequence; eventIds.add(event.eventId);
    const payload = event.payload;
    if (event.type === 'tool_call') {
      const expected = contract.commands[nextCall];
      if (!expected || nextCall !== nextResult || typeof payload?.callId !== 'string' || !payload.callId
          || calls.has(payload.callId) || payload.toolName !== 'bash' || payload.normalizedInput?.command !== expected.command) {
        throw new Error('experience command contract failed');
      }
      calls.set(payload.callId, nextCall++);
    } else if (event.type === 'tool_result') {
      if (calls.get(payload?.callId) !== nextResult || payload?.toolName !== 'bash'
          || !['successWithOutput', 'successNoOutput'].includes(payload.resultState)
          || typeof payload.modelContent !== 'string' || !payload.modelContent.includes(contract.commands[nextResult]?.output)) {
        throw new Error('experience tool output contract failed');
      }
      nextResult++;
    } else if (event.type === 'assistant_message') {
      if (payload?.status === 'done') finalText = payload.modelMarkdown;
    } else if (event.type === 'artifact_published') throw new Error('unexpected experience publication');
    else if (['agent_run_completed', 'agent_run_failed', 'agent_run_interrupted'].includes(event.type)) {
      if (event.type !== 'agent_run_completed') throw new Error('experience terminal failed');
      completed = true;
    }
  }
  if (!completed || nextCall !== contract.commands.length || nextResult !== nextCall
      || typeof finalText !== 'string' || finalText.trim() !== contract.finalText) throw new Error('experience result contract failed');
  return { toolCount: nextResult };
}

// Called only after the complete command/output/order contract passes. Project
// explicit identity/numeric/enum fields so model commands and output cannot leak.
function verifiedToolTimings(result, expectedCount) {
  if (!Array.isArray(result.toolTimings) || result.toolTimings.length !== expectedCount) throw new Error('per-tool observations missing');
  const calls = result.events.filter(event => event.type === 'tool_call' && event.payload.toolName === 'bash');
  const eventTime = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
  return result.toolTimings.map((timing, index) => {
    const call = calls[index];
    const completion = result.events.find(event => event.type === 'tool_result' && event.payload.callId === call?.payload.callId);
    const callCreated = eventTime(call?.createdAtMs), resultCreated = eventTime(completion?.createdAtMs);
    const serverState = callCreated !== null && resultCreated !== null && resultCreated >= callCreated ? 'observed' : 'notObserved';
    const callObserved = timing?.toolCallObservedAtMs, resultObserved = timing?.toolResultObservedAtMs;
    if (!Number.isSafeInteger(callObserved) || callObserved < 0 || !Number.isSafeInteger(resultObserved) || resultObserved < 0) throw new Error('client callback times missing');
    const clientState = resultObserved >= callObserved ? 'observed' : 'notObserved';
    const clientInterval = clientState === 'observed' ? resultObserved - callObserved : null;
    if (!timing || typeof timing.callId !== 'string' || !timing.callId || timing.callId.length > 160
        || timing.callId !== call?.payload.callId || timing.toolIndex !== index + 1 || timing.toolName !== 'bash'
        || timing.resultState !== completion?.payload.resultState
        || timing.toolCallCreatedAtMs !== callCreated || timing.toolResultCreatedAtMs !== resultCreated
        || timing.serverTimingState !== serverState || timing.clientTimingState !== clientState
        || timing.clientCallToResultMs !== clientInterval) throw new Error('per-tool timing binding invalid');
    return { callId: timing.callId, toolIndex: timing.toolIndex, toolName: 'bash',
      toolCallCreatedAtMs: callCreated, toolResultCreatedAtMs: resultCreated,
      toolCallObservedAtMs: callObserved, toolResultObservedAtMs: resultObserved,
      clientCallToResultMs: clientInterval, resultState: timing.resultState,
      serverTimingState: serverState, clientTimingState: clientState, contractState: 'verified' };
  });
}

function record(phase, data, sampleId, admitted, extra = {}) {
  console.log('PERF_EXPERIENCE ' + JSON.stringify({ schema: 'workspace.perf.experience.v1', phase, task,
    sampleId, operationId: sampleId, workspaceId: data.workspaceId, modelConfigRef: data.modelId,
    sessionId: admitted?.sessionId, agentRunId: admitted?.agentRunId, ...extra }));
}

export default function (data) {
  const sampleId = Array.from(new Uint8Array(crypto.randomBytes(16)), byte => byte.toString(16).padStart(2, '0')).join('');
  if (previousFailed) { failures.add(1); record('notSubmitted', data, sampleId, null, { errorCode: 'previous_sample_failed' }); return; }
  const contract = experienceTask(task, sampleId), started = Date.now();
  let admitted, stage = 'admission';
  try {
    admitted = startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.modelId, contract.prompt,
      { sessionId: sameSession === 'true' ? currentSession : 'new', operationId: sampleId, sampleId });
    currentSession = admitted.sessionId;
    const admissionMs = Date.now() - started; admission.add(admissionMs, { task });
    record('accepted', data, sampleId, admitted, { admissionMs, submittedAtMs: started });
    stage = 'terminal';
    const result = waitForTerminalObserved(data.store, data.csrf, admitted.sessionId, admitted.agentRunId, timeoutSeconds);
    stage = 'contract';
    const proof = verifyExperienceRun(result, contract, admitted.sessionId, admitted.agentRunId);
    const toolTimings = verifiedToolTimings(result, proof.toolCount);
    const times = {}, fields = [
      ['streamOpenedAtMs', 'streamOpenMs', streamOpened], ['firstSseItemObservedAtMs', 'firstSseItemMs', firstSseItem],
      ['firstModelTextObservedAtMs', 'firstModelTextMs', firstModelText],
      ['firstToolCallObservedAtMs', 'firstToolCallObservedMs', firstToolCall],
      ['firstSuccessfulToolResultObservedAtMs', 'firstSuccessfulToolResultMs', firstToolResult],
      ['terminalObservedAtMs', 'terminalMs', terminal],
    ];
    for (const [field, name, metric] of fields) {
      const value = result[field];
      if (value === null && (field === 'streamOpenedAtMs' || (!contract.commands.length && field.includes('Tool')))) { times[name] = null; continue; }
      if (!Number.isFinite(value) || value < started) throw new Error('experience receive time unavailable');
      times[name] = value - started;
      metric.add(times[name], { task, ...(name === 'firstModelTextMs' ? { source: result.firstModelTextSource } : {}) });
    }
    for (const timing of toolTimings) {
      if (timing.clientCallToResultMs !== null) toolClientInterval.add(timing.clientCallToResultMs, { task, toolIndex: String(timing.toolIndex) });
      record('toolTiming', data, sampleId, admitted, timing);
    }
    const secondToolClientCallToResultMs = toolTimings.length === 2 ? toolTimings[1].clientCallToResultMs : null;
    if (secondToolClientCallToResultMs !== null) secondToolClientInterval.add(secondToolClientCallToResultMs, { task });
    const verifiedMs = Date.now() - started;
    verifiedTime.add(verifiedMs, { task }); verified.add(1, { task }); failures.add(0, { task });
    record('verified', data, sampleId, admitted, { ...times, ...proof, admissionMs, verifiedMs,
      firstSseItemSource: result.firstSseItemSource, firstModelTextSource: result.firstModelTextSource,
      firstModelTextTimingKind: 'runStartToFirstObservedAssistantText', secondToolClientCallToResultMs,
      artifactDownloadMs: null, cleanup: 'notObserved' });
  } catch (_) {
    previousFailed = true; failures.add(1, { task }); record('failed', data, sampleId, admitted, { errorCode: 'experience_' + stage + '_failed' });
    throw new Error('experience sample failed at ' + stage);
  }
}
