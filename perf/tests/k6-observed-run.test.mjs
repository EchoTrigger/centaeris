import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

function observe({ foreign = false, failed = false, truncated = false, transportError = false, non200 = false, prefix = [], assistant = false, eventsOverride = null, observedTimes = null, frameCreatedAtMs } = {}) {
  let clock = 1000, closed = false; const handlers = {};
  const events = (eventsOverride || [
    { type: 'tool_call', payload: { callId: 'call', toolName: 'bash' } },
    { type: 'tool_result', payload: { callId: 'call', toolName: 'bash', resultState: failed ? 'error' : 'successWithOutput' } },
    ...(assistant ? [{ type: 'assistant_message', payload: { modelMarkdown: 'done', status: 'done' } }] : []),
    { type: 'agent_run_completed', payload: {} },
  ]).map((event, index) => ({ ...event, sequence: index + 1, eventId: 'event-' + index,
    sessionId: 'session', agentRunId: foreign ? 'other' : 'run' }));
  const items = [...prefix, ...events.map(event => ({ schema: 'session.stream.item.v1', kind: 'committed',
    agentRunId: event.agentRunId, sourceSequence: event.sequence, createdAtMs: frameCreatedAtMs, event }))];
  const context = vm.createContext({ __ENV: {}, Date: { now: () => clock }, crypto: {}, sleep() {},
    http: { get() { clock = 9000; return { status: 200, body: items.map(item => 'data: ' + JSON.stringify(item)).join('\n\n') }; } },
    sse: { open(url, options, configure) {
      if (non200) return { status: 503 };
      assert.equal(options.timeout, '120s'); assert.equal(options.headers.Cookie, 'sessionid=owned');
      configure({ on(kind, callback) { handlers[kind] = callback; }, close() { closed = true; } });
      clock = 1100; handlers.open?.();
      if (transportError) { handlers.error?.(); return { status: 200 }; }
      for (let index = 0; index < (truncated ? 2 : items.length) && !closed; index += 1) {
        clock = observedTimes ? observedTimes[index] : (prefix.length || assistant ? 1200 + index * 100 : [1200, 2500, 8000][index]);
        handlers.event?.({ data: JSON.stringify(items[index]) });
      }
      clock = 9000; return { status: 200 };
    } },
  });
  const source = fs.readFileSync(new URL('../k6/scenarios/common.js', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  vm.runInContext(source, context);
  const subject = context.waitForTerminalObserved || context.waitForTerminal;
  return { result: subject({ sessionid: 'owned' }, '', 'session', 'run', 120), closed };
}

test('first successful tool result is timestamped at its callback before terminal/response end', () => {
  const { result, closed } = observe();
  assert.equal(result.status, 'completed');
  assert.equal(result.firstSuccessfulToolResultObservedAtMs, 2500);
  assert.equal(result.firstCommittedEventObservedAtMs, 1200);
  assert.equal(result.streamOpenedAtMs, 1100);
  assert.equal(result.firstSseItemObservedAtMs, 1200);
  assert.equal(result.firstSseItemSource, 'committed');
  assert.equal(result.firstToolCallObservedAtMs, 1200);
  assert.equal(result.terminalObservedAtMs, 8000);
  assert.equal(closed, true);
});

function live(extra = {}) {
  return { schema: 'session.stream.item.v1', kind: 'live', agentRunId: 'run',
    afterSequence: 0, revision: 1, turnId: 'turn', messageId: 'message:turn:assistant', text: '', ...extra };
}

test('first model text uses the receive callback of nonempty live text, not reasoning or stream open', () => {
  const { result } = observe({ prefix: [live({ reasoning: { text: 'thinking' } }), live({ revision: 2, text: 'hello' })], assistant: true });
  assert.equal(result.firstSseItemObservedAtMs, 1200);
  assert.equal(result.firstSseItemSource, 'live');
  assert.equal(result.firstModelTextObservedAtMs, 1300);
  assert.equal(result.firstModelTextSource, 'live');
  assert.equal(result.firstCommittedEventObservedAtMs, 1400);
});

test('committed assistant text is an explicit fallback, while no text remains unobserved', () => {
  const { result } = observe({ assistant: true });
  assert.equal(result.firstModelTextObservedAtMs, 1400);
  assert.equal(result.firstModelTextSource, 'committedAssistantMessage');
  assert.equal(observe().result.firstModelTextObservedAtMs, null);
  assert.equal(observe().result.firstModelTextSource, null);
});

test('live items must bind to this Run and carry a valid overlay position before supplying timings', () => {
  for (const change of [{ agentRunId: 'foreign' }, { afterSequence: -1 }, { revision: 0 }, { messageId: '' }, { text: null }]) {
    assert.throws(() => observe({ prefix: [live({ text: 'hello', ...change })] }));
  }
});
test('failed tool result is never reported as first successful execution', () => {
  assert.equal(observe({ failed: true }).result.firstSuccessfulToolResultObservedAtMs, null);
});
test('foreign binding and missing terminal are rejected', () => {
  assert.throws(() => observe({ foreign: true }));
  assert.throws(() => observe({ truncated: true }));
});

test('transport error and non-200 initial connection fail without successful timing', () => {
  assert.throws(() => observe({ transportError: true }));
  assert.throws(() => observe({ non200: true }));
});


function bashEvent(type, callId, createdAtMs) {
  return { type, createdAtMs, payload: { callId, toolName: 'bash', ...(type === 'tool_result' ? { resultState: 'successWithOutput' } : {}) } };
}

test('every actual bash pair keeps independent server event and client receive clocks', () => {
  const { result } = observe({ eventsOverride: [bashEvent('tool_call', 'first', 1000000), bashEvent('tool_result', 'first', 1000010),
    bashEvent('tool_call', 'second', 2000000), bashEvent('tool_result', 'second', 2000005),
    { type: 'assistant_message', payload: { modelMarkdown: 'final answer' } }, { type: 'agent_run_completed' }],
    observedTimes: [1200, 1800, 4000, 4050, 7000, 8000] });
  assert.equal(result.firstSuccessfulToolResultObservedAtMs, 1800);
  assert.equal(result.firstModelTextObservedAtMs, 7000);
  assert.deepEqual(JSON.parse(JSON.stringify(result.toolTimings)), [
    { callId: 'first', toolIndex: 1, toolName: 'bash', toolCallCreatedAtMs: 1000000, toolResultCreatedAtMs: 1000010,
      toolCallObservedAtMs: 1200, toolResultObservedAtMs: 1800, clientCallToResultMs: 600,
      resultState: 'successWithOutput', serverTimingState: 'observed', clientTimingState: 'observed' },
    { callId: 'second', toolIndex: 2, toolName: 'bash', toolCallCreatedAtMs: 2000000, toolResultCreatedAtMs: 2000005,
      toolCallObservedAtMs: 4000, toolResultObservedAtMs: 4050, clientCallToResultMs: 50,
      resultState: 'successWithOutput', serverTimingState: 'observed', clientTimingState: 'observed' },
  ]);
});

test('missing or invalid server event times remain null and envelope time cannot replace them', () => {
  for (const invalid of [undefined, null, '1000', -1, 1.5]) {
    const { result } = observe({ eventsOverride: [bashEvent('tool_call', 'call', invalid), bashEvent('tool_result', 'call', 10),
      { type: 'agent_run_completed' }], frameCreatedAtMs: 9999999 });
    assert.equal(result.toolTimings[0].toolCallCreatedAtMs, null);
    assert.equal(result.toolTimings[0].toolResultCreatedAtMs, 10);
    assert.equal(result.toolTimings[0].serverTimingState, 'notObserved');
    assert.equal(result.toolTimings[0].clientCallToResultMs, 1300);
  }
});

test('clock rollback is never converted to a valid timing interval', () => {
  const { result } = observe({ eventsOverride: [bashEvent('tool_call', 'call', 100), bashEvent('tool_result', 'call', 90),
    { type: 'agent_run_completed' }], observedTimes: [1200, 1190, 8000] });
  assert.equal(result.toolTimings[0].toolCallCreatedAtMs, 100);
  assert.equal(result.toolTimings[0].toolResultCreatedAtMs, 90);
  assert.equal(result.toolTimings[0].serverTimingState, 'notObserved');
  assert.equal(result.toolTimings[0].clientCallToResultMs, null);
  assert.equal(result.toolTimings[0].clientTimingState, 'notObserved');
});

test('orphaned or duplicate bash results cannot supply per-call timings', () => {
  for (const eventsOverride of [
    [bashEvent('tool_result', 'orphan', 100), { type: 'agent_run_completed' }],
    [bashEvent('tool_call', 'call', 100), bashEvent('tool_result', 'call', 110), bashEvent('tool_result', 'call', 120), { type: 'agent_run_completed' }],
  ]) assert.throws(() => observe({ eventsOverride, observedTimes: [1200, 1300, 1400, 8000] }));
});
