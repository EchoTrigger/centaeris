import { committedItems } from './session-stream-fixtures.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source = fs.readFileSync(new URL('../k6/scenarios/common.js', import.meta.url), 'utf8')
  .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
const context = vm.createContext({ __ENV: {}, Date, http: {}, crypto: {}, sleep() {} });
vm.runInContext(source, context);
function event(type, payload, sequence) {
  return { type, payload, sequence, eventId: `event-${sequence}`, sessionId: 'session-owned', agentRunId: 'run-owned' };
}
function completed() {
  const events = [
    event('tool_call', { callId: 'call-owned', toolName: 'bash' }, 1),
    event('tool_result', { callId: 'call-owned', toolName: 'bash', resultState: 'successWithOutput', modelContent: 'actual output' }, 2),
    event('agent_run_completed', {}, 3),
  ];
  return { status: 'completed', events: committedItems(events).map(item => item.event) };
}
test('completed Run requires real paired successful bash results', () => {
  assert.equal(context.verifyToolRun(completed(), 'session-owned', 'run-owned').toolCount, 1);
});
test('failed tool cannot pass when the model still completes the Run', () => {
  const value = completed(); value.events[1].payload.resultState = 'failed';
  assert.throws(() => context.verifyToolRun(value, 'session-owned', 'run-owned'), /tool failed/);
});
test('missing, duplicate, foreign or mismatched call evidence is rejected', () => {
  const changes = [
    value => { value.events.splice(1, 1); },
    value => { value.events.push(value.events[1]); },
    value => { value.events[1].payload.callId = 'another-call'; },
    value => { value.events[1].agentRunId = 'foreign-run'; },
    value => { value.events[1].payload.toolName = 'another-tool'; },
    value => { value.events = [value.events[2]]; },
  ];
  for (const change of changes) { const value = completed(); change(value); assert.throws(() => context.verifyToolRun(value, 'session-owned', 'run-owned')); }
});
