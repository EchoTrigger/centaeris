import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController.ts';
import { readPresentationSseBlock } from '../../src/chat/workspacePresentationProtocol.ts';
import { streamWorkspacePresentation } from '../../src/chat/workspacePresentationTransport.ts';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore.ts';
import { ApiError } from '../../src/api.ts';

const identity = { workspaceId: 'workspace', agentId: 'agent', sessionId: 'session' };
const envelope = (kind, extra = {}) => ({ schema: 'workspace.presentation.v1', ...identity, kind, ...extra });
const entry = (kind, extra = {}, cursor = null) => ({ cursor, item: envelope(kind, extra) });
const page = (blocks = [], water = '0', generation = 'generation') => ({
  schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
  projectionGeneration: generation, sourceHighWater: water, blocks, olderCursor: null, hasOlder: false,
  resumeCursors: water === '0' ? [] : [{ streamId: 'workspace-transcript.v1', cursor: water }],
});
const block = (text = 'Committed answer', content = { inlineContent: text, sourceRef: null }) => ({
  blockId: 'answer', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 0 },
  body: { kind: 'assistantText', content, status: 'completed' },
});
const patches = (blocks, generation = 'generation') => ({
  schema: 'transcript.patch.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
  projectionGeneration: generation, throughSourceHighWater: '1', nextSourceHighWater: '1', hasMore: false,
  patches: [{ schema: 'transcript.patch.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
    projectionGeneration: generation, sourceHighWater: '1', streamId: 'workspace-transcript.v1', appliedCursor: '1',
    upserts: blocks, removals: [] }],
});
const checkpoint = (water = '0', generation = 'generation', status = 'running') => ({
  projectionGeneration: generation, sourceHighWater: water,
  agentRun: { agentRunId: 'run', status, startedAt: '2026-10-23T10:00:00Z', completedAt: status === 'running' ? null : '2026-10-23T10:00:01Z' },
});
const live = (text = 'Live answer') => ({ agentRunId: 'run', live: { afterSequence: 0, revision: 1, turnId: 'turn', messageId: 'answer', text, reasoning: null } });
const wire = (frames) => frames.map(({ item, cursor }) => `${cursor === null ? '' : `id: ${cursor}\n`}data: ${JSON.stringify(item)}\n\n`).join('');
function fixture(options = {}) {
  const store = createTranscriptViewStore();
  store.openTail(page());
  const checkpoints = [];
  const controller = new WorkspacePresentationController({ ...identity, store, onCheckpoint: value => checkpoints.push(value), ...options });
  return { store, checkpoints, controller };
}

test('presentation protocol rejects unknown fields, mismatched binding and non-checkpoint cursors', () => {
  assert.deepEqual(readPresentationSseBlock(wire([entry('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [] })]), identity).item, envelope('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [] }));
  for (const value of [entry('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [], unexpected: true }), { ...entry('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [] }), item: { ...envelope('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [] }), agentId: 'other' } }, entry('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [] }, 'received')]) {
    assert.throws(() => readPresentationSseBlock(wire([value]), identity));
  }
  assert.equal(readPresentationSseBlock(': heartbeat\n\n', identity), null);
});

test('direct snapshot and patch frames render without any follow-up HTTP request', async () => {
  const { store, controller } = fixture();
  await controller.acceptWithBackpressure(entry('snapshot', { page: page() }));
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block()]) }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1'), 'applied:1'));
  await controller.whenIdle();
  assert.equal(store.getBlockSnapshot('answer').body.content.inlineContent, 'Committed answer');
  assert.equal(controller.lastCursor, 'applied:1');
});

test('server terminal metadata stops the timer before projection catches up and retains live text', async () => {
  const { store, controller, checkpoints } = fixture();
  await controller.acceptWithBackpressure(entry('snapshot', { page: page() }));
  await controller.acceptWithBackpressure(entry('live', live()));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1', 'generation', 'completed')));
  await controller.whenIdle();
  assert.equal(checkpoints.at(-1).agentRun.status, 'completed');
  assert.equal(store.getLiveSnapshot().text, 'Live answer');
  assert.equal(controller.lastCursor, null, 'an unacknowledged checkpoint cannot skip missing content');
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block()]) }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1', 'generation', 'completed'), 'applied:1'));
  await controller.whenIdle();
  assert.equal(store.getLiveSnapshot(), null);
  assert.equal(controller.lastCursor, 'applied:1');
});

test('failed application never advances the resume cursor and replay remains idempotent', async () => {
  const { store, controller } = fixture();
  await controller.acceptWithBackpressure(entry('snapshot', { page: page() }));
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block()]) }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1'), 'applied:1'));
  await controller.whenIdle();
  const conflict = entry('patches', { page: { ...patches([block('Conflicting same revision')]), throughSourceHighWater: '2', nextSourceHighWater: '2', patches: [{ ...patches([block()]).patches[0], sourceHighWater: '2', appliedCursor: '2', upserts: [block('Conflicting same revision')] }] } });
  await assert.rejects(controller.acceptWithBackpressure(conflict));
  assert.equal(controller.lastCursor, 'applied:1');
  assert.equal(store.getBlockSnapshot('answer').body.content.inlineContent, 'Committed answer');
});

test('reset preserves readable history until a valid replacement snapshot applies', async () => {
  const { store, controller } = fixture();
  await controller.acceptWithBackpressure(entry('snapshot', { page: page([block()], '1') }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1'), 'old:1'));
  await controller.acceptWithBackpressure(entry('reset', { reason: 'projectionInvalidated' }));
  assert.equal(store.getBlockSnapshot('answer').body.content.inlineContent, 'Committed answer');
  assert.equal(controller.lastCursor, null);
  await controller.acceptWithBackpressure(entry('snapshot', { page: page([block('Fresh answer')], '1', 'new-generation') }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1', 'new-generation'), 'new:1'));
  assert.equal(store.getBlockSnapshot('answer').body.content.inlineContent, 'Fresh answer');
  assert.equal(controller.lastCursor, 'new:1');
});

test('a late frame cannot write into another selected view', async () => {
  let current = true;
  const { store, controller } = fixture({ isCurrent: () => current });
  await controller.acceptWithBackpressure(entry('snapshot', { page: page() }));
  current = false;
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block()]) }));
  assert.equal(store.getBlockSnapshot('answer'), null);
  assert.equal(controller.lastCursor, null);
});

test('pushed normal content appears progressively and the applied cursor waits for complete referenced bytes', async () => {
  const { store, controller } = fixture();
  const reference = { refId: 'session-event:answer', revision: '1', byteLength: '6' };
  await controller.acceptWithBackpressure(entry('snapshot', { page: page([block('', { inlineContent: null, sourceRef: reference })], '1') }));
  const content = (startOffset, endOffset, text, hasMore) => entry('content', { range: {
    schema: 'transcript.content.range.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation',
    ...reference, startOffset, endOffset, content: text, hasMore,
  } });
  await controller.acceptWithBackpressure(content('0', '3', 'abc', true));
  assert.equal(store.getPresentationContentSnapshot(reference).content, 'abc');
  await assert.rejects(controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1'), 'too-early')), /content/);
  assert.equal(controller.lastCursor, null);
});

test('clean EOF reconnects from the applied cursor and does not mark a running task failed', async () => {
  const { controller } = fixture();
  const abort = new AbortController();
  const paths = [], cursors = [], delays = [];
  await streamWorkspacePresentation({ ...identity, controller, signal: abort.signal, onConnection() {},
    request: async (path, options) => {
      paths.push(path); cursors.push(options.headers?.['Last-Event-ID']);
      if (paths.length === 1) return new Response(wire([entry('snapshot', { page: page() }), entry('checkpoint', checkpoint(), 'applied:0')]), { headers: { 'content-type': 'text/event-stream' } });
      abort.abort();
      throw new DOMException('Aborted', 'AbortError');
    }, wait: async ms => { delays.push(ms); },
  }).catch(error => { assert.equal(error.name, 'AbortError'); });
  assert.equal(paths.length, 2);
  assert.equal(paths[0], '/api/workspaces/workspace/presentation/events?agentId=agent&sessionId=session');
  assert.deepEqual(cursors, [undefined, 'applied:0']);
  assert.equal(delays.length, 1);
});

for (const stage of ['headers', 'body']) test(`${stage} silence has a deadline and releases the attempt before reconnect`, async () => {
  const { controller } = fixture();
  const abort = new AbortController();
  let requests = 0, firstSignal, cancelled = false;
  const promise = streamWorkspacePresentation({ ...identity, controller, signal: abort.signal, onConnection() {}, headersTimeoutMs: 5, silenceTimeoutMs: 5,
    request: async (_path, options) => {
      requests++;
      if (requests > 1) { abort.abort(); throw new DOMException('Aborted', 'AbortError'); }
      firstSignal = options.signal;
      if (stage === 'headers') return new Promise(() => {});
      return new Response(new ReadableStream({ cancel() { cancelled = true; } }), { headers: { 'content-type': 'text/event-stream' } });
    }, wait: async () => {},
  });
  await assert.rejects(promise, { name: 'AbortError' });
  assert.equal(firstSignal.aborted, true);
  assert.equal(requests, 2);
  if (stage === 'body') assert.equal(cancelled, true);
});

test('authorization rejection is permanent and never reconnects', async () => {
  const { controller } = fixture();
  let requests = 0;
  await assert.rejects(streamWorkspacePresentation({ ...identity, controller, signal: new AbortController().signal, onConnection() {},
    request: async () => { requests++; throw new ApiError('forbidden', 403); }, wait: async () => { throw new Error('must not retry'); },
  }), /forbidden/);
  assert.equal(requests, 1);
});


test('disconnect after applying a patch but before its checkpoint can replay safely from the previous cursor', async () => {
  const { controller, store } = fixture();
  await controller.acceptWithBackpressure(entry('snapshot', { page: page() }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint(), 'applied:0'));
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block()]) }));
  assert.equal(controller.lastCursor, 'applied:0');
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block()]) }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('1'), 'applied:1'));
  assert.equal(store.getBlockSnapshot('answer').body.content.inlineContent, 'Committed answer');
  assert.equal(controller.lastCursor, 'applied:1');
});

test('restoring loaded older history does not wait for bytes outside the pushed snapshot tail', async () => {
  const store = createTranscriptViewStore();
  const oldReference = { refId: 'session-event:historical', revision: '1', byteLength: '70000' };
  const old = { ...block('', { inlineContent: null, sourceRef: oldReference }), blockId: 'historical', orderKey: { sourceSequence: '1', ordinal: 0 } };
  const controller = new WorkspacePresentationController({ ...identity, store,
    applySnapshot: fresh => store.openTail({ ...fresh, blocks: [old, ...fresh.blocks] }),
  });
  await controller.acceptWithBackpressure(entry('snapshot', { page: page([{ ...block('Latest'), orderKey: { sourceSequence: '2', ordinal: 0 } }], '2') }));
  await controller.acceptWithBackpressure(entry('checkpoint', checkpoint('2'), 'applied:2'));
  assert.equal(store.getPresentationContentSnapshot(oldReference), null, 'older history retains its ordinary demand reader');
  assert.equal(controller.lastCursor, 'applied:2');
});

test('normal referenced answer retains live content through a terminal notice until its bytes arrive', async () => {
  const { store, controller } = fixture();
  const reference = { refId: 'session-event:answer', revision: '1', byteLength: '3' };
  await controller.acceptWithBackpressure(entry('snapshot', { page: page() }));
  await controller.acceptWithBackpressure(entry('live', live('abc')));
  const notice = { blockId: 'terminal', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 1 },
    body: { kind: 'notice', noticeType: 'run_boundary', status: 'completed', content: { inlineContent: 'Done', sourceRef: null } } };
  await controller.acceptWithBackpressure(entry('patches', { page: patches([block('', { inlineContent: null, sourceRef: reference }), notice]) }));
  assert.equal(store.getLiveSnapshot().text, 'abc');
  await controller.acceptWithBackpressure(entry('content', { range: { schema: 'transcript.content.range.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', ...reference, startOffset: '0', endOffset: '3', content: 'abc', hasMore: false } }));
  assert.equal(store.getLiveSnapshot(), null);
  assert.equal(store.getPresentationContentSnapshot(reference).content, 'abc');
});


test('immutable UTF-8 content replays idempotently and conflicting bytes fail without acknowledgment', async () => {
  const { store, controller } = fixture();
  const reference = { refId: 'session-event:unicode', revision: '1', byteLength: '6' };
  await controller.acceptWithBackpressure(entry('snapshot', { page: page([block('', { inlineContent: null, sourceRef: reference })], '1') }));
  const range = { schema: 'transcript.content.range.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', ...reference,
    startOffset: '0', endOffset: '6', content: '你好', hasMore: false };
  await controller.acceptWithBackpressure(entry('content', { range }));
  await controller.acceptWithBackpressure(entry('content', { range }));
  assert.equal(store.getPresentationContentSnapshot(reference).content, '你好');
  await assert.rejects(controller.acceptWithBackpressure(entry('content', { range: { ...range, content: '再见' } })), /conflicting/);
  assert.equal(controller.lastCursor, null);
});

test('repeated clean EOF without an applied checkpoint exhausts bounded retry instead of spinning forever', async () => {
  const { controller } = fixture();
  let requests = 0;
  const delays = [];
  await assert.rejects(streamWorkspacePresentation({ ...identity, controller, signal: new AbortController().signal, onConnection() {}, retryLimit: 2, random: () => 0.5,
    request: async () => { requests++; return new Response('', { headers: { 'content-type': 'text/event-stream' } }); },
    wait: async ms => { delays.push(ms); },
  }), /repeatedly ended/);
  assert.equal(requests, 3);
  assert.deepEqual(delays, [500, 1000]);
});

test('online recovery interrupts backoff and resumes the same applied view', async () => {
  const { controller } = fixture();
  controller.lastCursor = 'retained:cursor';
  const abort = new AbortController();
  const cursors = [];
  let wake, unsubscribed = false;
  await assert.rejects(streamWorkspacePresentation({ ...identity, controller, signal: abort.signal, onConnection() {},
    subscribeWake: callback => { wake = callback; return () => { unsubscribed = true; }; },
    request: async (_path, options) => {
      cursors.push(options.headers['Last-Event-ID']);
      if (cursors.length > 1) { abort.abort(); throw new DOMException('Aborted', 'AbortError'); }
      throw new TypeError('connection lost');
    },
    wait: (_ms, signal) => new Promise((resolve, reject) => {
      signal.addEventListener('abort', () => reject(signal.reason), { once: true });
      wake(true);
    }),
  }), { name: 'AbortError' });
  assert.deepEqual(cursors, ['retained:cursor', 'retained:cursor']);
  assert.equal(unsubscribed, true);
});

test('comment heartbeats keep a silent model connected without inventing task progress or advancing a cursor', async () => {
  const { controller, checkpoints } = fixture();
  const abort = new AbortController();
  let requests = 0, stream;
  const encoding = new TextEncoder();
  const pending = streamWorkspacePresentation({ ...identity, controller, signal: abort.signal, onConnection() {}, silenceTimeoutMs: 80,
    request: async () => { requests++; return new Response(new ReadableStream({ start(value) { stream = value; } }), { headers: { 'content-type': 'text/event-stream' } }); },
  });
  for (let index = 0; index < 6; index++) {
    await new Promise(resolve => setTimeout(resolve, 20));
    stream.enqueue(encoding.encode(': heartbeat\n\n'));
  }
  abort.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  assert.equal(requests, 1);
  assert.equal(controller.lastCursor, null);
  assert.equal(checkpoints.length, 0);
});


const navigationRow = id => ({ id, workspaceId: 'workspace', agentId: 'agent', projectId: null, currentEnvironmentId: null,
  executionMode: 'local', title: id, origin: 'user', initialInputOrigin: null, status: 'active', deletedAt: null,
  isPinned: false, isUnread: false, hasActiveAgentRun: false, updatedAt: '2026-10-23T10:00:00Z',
  latestAttentionSequence: '0', readThroughSequence: '0', attentionRevision: '0', isManuallyUnread: false });

test('navigation chunks publish one complete snapshot without truncating the existing session list', async () => {
  const published = [];
  const { controller } = fixture({ onNavigation: rows => published.push(rows) });
  await controller.acceptWithBackpressure(entry('navigation', { snapshotId: 'n1', offset: 0, hasMore: true, sessions: [navigationRow('one')] }));
  assert.equal(published.length, 0);
  await controller.acceptWithBackpressure(entry('navigation', { snapshotId: 'n1', offset: 1, hasMore: false, sessions: [navigationRow('two')] }));
  assert.deepEqual(published[0].map(row => row.id), ['one', 'two']);
  await controller.acceptWithBackpressure(entry('navigation', { snapshotId: 'partial', offset: 0, hasMore: true, sessions: [navigationRow('old')] }));
  await controller.acceptWithBackpressure(entry('navigation', { snapshotId: 'reconnected', offset: 0, hasMore: false, sessions: [navigationRow('fresh')] }));
  assert.deepEqual(published[1].map(row => row.id), ['fresh']);
});

test('an incomplete navigation snapshot cannot be acknowledged and unknown hosted run states are rejected', async () => {
  const { controller } = fixture();
  await controller.acceptWithBackpressure(entry('navigation', { snapshotId: 'n1', offset: 0, hasMore: true, sessions: [navigationRow('one')] }));
  await assert.rejects(controller.acceptWithBackpressure(entry('checkpoint', checkpoint(), 'too-soon')), /navigation/);
  assert.equal(controller.lastCursor, null);
  assert.throws(() => readPresentationSseBlock(wire([entry('checkpoint', checkpoint('0', 'generation', 'interrupted'))]), identity));
  assert.doesNotThrow(() => readPresentationSseBlock(wire([entry('checkpoint', checkpoint('0', 'generation', 'cancelled'))]), identity));
});

test('bounded tool details apply before their delivery checkpoint and reject protocol extensions', async () => {
  const operations = [];
  const { controller } = fixture({ onOperation: detail => operations.push(detail) });
  const detail = { kind: 'call', callId: 'call', toolName: 'exec_command', normalizedInput: { command: 'pwd' },
    resultState: null, operations: [], previewTruncated: false, sourceEventId: 'event:call' };
  await controller.acceptWithBackpressure(entry('operation', { detail }));
  assert.deepEqual(operations, [detail]);
  assert.throws(() => readPresentationSseBlock(wire([entry('operation', { detail: { ...detail, modelContent: 'must not travel here' } })]), identity));
  assert.throws(() => readPresentationSseBlock(wire([entry('operation', { detail: { ...detail, normalizedInput: { command: 'x'.repeat(512 * 1024) } } })]), identity));
});


test('the API emitted public presentation fixtures are accepted without a second hand-maintained schema', async () => {
  const fixture = JSON.parse(readFileSync(new URL('../../../../tests/fixtures/workspace-presentation.json', import.meta.url), 'utf8'));
  assert.equal(fixture.schema, 'workspace.presentation.fixtures.v1');
  for (const item of fixture.frames) {
    const cursor = item.kind === 'checkpoint' ? fixture.initialCursor : null;
    assert.deepEqual(readPresentationSseBlock(wire([{ cursor, item }]), fixture.scope), { cursor, item });
  }
  const store = createTranscriptViewStore();
  const controller = new WorkspacePresentationController({ ...fixture.scope, store });
  for (let index = 0; index < fixture.initialFrames.length; index++) {
    const item = fixture.initialFrames[index];
    const cursor = index === fixture.initialFrames.length - 1 ? fixture.initialCursor : null;
    await controller.acceptWithBackpressure(readPresentationSseBlock(wire([{ cursor, item }]), fixture.scope));
  }
  assert.equal(controller.lastCursor, fixture.initialCursor);
  assert.equal(store.getBlockSnapshot('message').body.content.inlineContent, 'Ready');
});

test('stable navigation-only connections replenish consecutive retries after sustained liveness', async () => {
  const store = createTranscriptViewStore();
  const controller = new WorkspacePresentationController({ ...identity, sessionId: null, store });
  const abort = new AbortController();
  const delays = [];
  let requests = 0, now = 0;
  await assert.rejects(streamWorkspacePresentation({ ...identity, sessionId: null, controller, signal: abort.signal, now: () => now, random: () => 0.5,
    onConnection() {}, retryLimit: 1,
    request: async () => {
      if (++requests === 4) { abort.abort(); throw new DOMException('Aborted', 'AbortError'); }
      const item = { ...envelope('navigation', { snapshotId: 'navigation', offset: 0, hasMore: false, sessions: [] }), sessionId: null };
      return new Response(new ReadableStream({ start(writer) { setImmediate(() => {
        now += 30_001; writer.enqueue(new TextEncoder().encode(wire([{ cursor: null, item }]))); writer.close();
      }); } }), { headers: { 'content-type': 'text/event-stream' } });
    }, wait: async delay => { delays.push(delay); now += delay; },
  }), { name: 'AbortError' });
  assert.equal(requests, 4);
  assert.deepEqual(delays, [500, 500, 500]);
});

test('visibility wake reconnects only a stale connection and fresh heartbeat bytes preserve the current stream', async context => {
  const { controller } = fixture();
  controller.lastCursor = 'retained:cursor';
  const abort = new AbortController();
  const cursors = [];
  let now = 0, wake, writer;
  context.mock.method(Date, 'now', () => now);
  const reading = streamWorkspacePresentation({ ...identity, controller, signal: abort.signal, onConnection() {},
    subscribeWake: callback => { wake = callback; return () => {}; },
    request: async (_path, options) => {
      cursors.push(options.headers['Last-Event-ID']);
      return new Response(new ReadableStream({ start(value) { writer = value; } }), { headers: { 'content-type': 'text/event-stream' } });
    }, wait: async () => {},
  });
  const tick = () => new Promise(resolve => setImmediate(resolve));
  await tick();
  wake(false); await tick();
  assert.equal(cursors.length, 1, 'visible focus alone does not reconnect a fresh connection');
  now = 12000;
  writer.enqueue(new TextEncoder().encode(': heartbeat\n\n'));
  await tick();
  now = 15000;
  wake(false); await tick();
  assert.equal(cursors.length, 1, 'a recent heartbeat keeps the connection fresh without task progress');
  now = 22001;
  wake(false); await tick();
  assert.deepEqual(cursors, ['retained:cursor', 'retained:cursor']);
  abort.abort();
  await assert.rejects(reading, { name: 'AbortError' });
});

test('browser online and visibility listeners are removed when their subscription is cancelled', async context => {
  class Surface extends EventTarget {
    registered = new Map();
    addEventListener(type, callback, options) { super.addEventListener(type, callback, options); this.registered.set(type, callback); }
    removeEventListener(type, callback, options) { super.removeEventListener(type, callback, options); this.registered.delete(type); }
  }
  const previousWindow = globalThis.window, previousDocument = globalThis.document;
  const windowSurface = new Surface(), documentSurface = Object.assign(new Surface(), { visibilityState: 'visible' });
  globalThis.window = windowSurface; globalThis.document = documentSurface;
  context.after(() => { globalThis.window = previousWindow; globalThis.document = previousDocument; });
  const { controller } = fixture();
  const abort = new AbortController();
  let requests = 0;
  const reading = streamWorkspacePresentation({ ...identity, controller, signal: abort.signal, onConnection() {},
    request: async () => { requests++; return new Response(new ReadableStream({}), { headers: { 'content-type': 'text/event-stream' } }); },
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(windowSurface.registered.has('online'), true);
  assert.equal(documentSurface.registered.has('visibilitychange'), true);
  abort.abort();
  await assert.rejects(reading, { name: 'AbortError' });
  assert.equal(windowSurface.registered.size, 0);
  assert.equal(documentSurface.registered.size, 0);
  windowSurface.dispatchEvent(new Event('online'));
  documentSurface.dispatchEvent(new Event('visibilitychange'));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(requests, 1, 'a disposed subscription cannot wake itself again');
});

test('invalid derived notification state fails at the actual SSE boundary before navigation is published', async () => {
  const published = [];
  const { controller } = fixture({ onNavigation: rows => published.push(rows) });
  const malformed = entry('navigation', { snapshotId: 'bad-attention', offset: 0, hasMore: false,
    sessions: [{ ...navigationRow('unread'), latestAttentionSequence: '5', readThroughSequence: '0', isUnread: false }] });
  assert.throws(() => readPresentationSseBlock(wire([malformed]), identity), /attention|navigation/);
  await assert.rejects(controller.acceptWithBackpressure(malformed), /attention|navigation/);
  assert.equal(published.length, 0);
  assert.equal(controller.lastCursor, null);
});


test('workspace navigation retains Sessions from retired and other Agents without weakening frame identity', () => {
  const value = entry('navigation', { snapshotId: 'workspace-sessions', offset: 0, hasMore: false,
    sessions: [navigationRow('current'), { ...navigationRow('retired-history'), agentId: 'retired-agent' }] });
  const parsed = readPresentationSseBlock(wire([value]), identity);
  assert.deepEqual(parsed.item.sessions.map(session => session.agentId), ['agent', 'retired-agent']);
  assert.throws(() => readPresentationSseBlock(wire([{ ...value, item: { ...value.item, agentId: 'different-subscription' } }]), identity));
  assert.throws(() => readPresentationSseBlock(wire([entry('navigation', { snapshotId: 'foreign', offset: 0, hasMore: false,
    sessions: [{ ...navigationRow('foreign-history'), workspaceId: 'foreign' }] })]), identity));
});
