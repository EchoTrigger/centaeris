import assert from 'node:assert/strict';
import test from 'node:test';
import { ApiError, apiResponse, configureApi } from '../../src/api.ts';
import { streamWorkspacePresentation } from '../../src/chat/workspacePresentationTransport.ts';

const identity = { workspaceId: 'workspace', agentId: 'agent', sessionId: null };
const encoder = new TextEncoder();
const tick = () => new Promise(resolve => setImmediate(resolve));
const navigation = () => `data: ${JSON.stringify({ schema: 'workspace.presentation.v1', ...identity, kind: 'navigation', snapshotId: 'same', offset: 0, hasMore: false, sessions: [] })}\n\n`;
const checkpoint = () => `id: retained\ndata: ${JSON.stringify({ schema: 'workspace.presentation.v1', ...identity, sessionId: 'session', kind: 'checkpoint', projectionGeneration: 'generation', sourceHighWater: '0', agentRun: null })}\n\n`;
const response = text => new Response(text, { headers: { 'content-type': 'text/event-stream' } });
const controller = () => ({ lastCursor: null, async acceptWithBackpressure(entry) { if (entry.cursor !== null) this.lastCursor = entry.cursor; } });
const base = () => ({ ...identity, controller: controller(), signal: new AbortController().signal, onConnection() {}, now: () => 0, random: () => 0.5, wait: async () => {}, subscribeWake: () => () => {} });

test('the same applied navigation snapshot cannot purchase unlimited automatic reconnects', async () => {
  let requests = 0;
  await assert.rejects(streamWorkspacePresentation({ ...base(), retryLimit: 2,
    request: async () => { if (++requests > 8) throw new ApiError('regression guard', 403); return response(navigation()); },
  }), /budget|repeatedly ended/);
  assert.equal(requests, 3, 'the initial connection plus two automatic recoveries exhaust the budget');
});

test('repeated checkpoints do not replenish the connection budget either', async () => {
  let requests = 0;
  const resumed = [];
  await assert.rejects(streamWorkspacePresentation({ ...base(), sessionId: 'session', retryLimit: 2,
    request: async (_path, options) => { resumed.push(options.headers['Last-Event-ID']); if (++requests > 8) throw new ApiError('regression guard', 403); return response(checkpoint()); },
  }), /budget|repeatedly ended/);
  assert.equal(requests, 3);
  assert.deepEqual(resumed, [undefined, 'retained', 'retained']);
});

test('online wakeups remain inside the total automatic retry budget', async () => {
  let wake, requests = 0;
  const seen = [];
  await assert.rejects(streamWorkspacePresentation({ ...base(), retryLimit: 10,
    subscribeWake: callback => { wake = callback; return () => {}; },
    request: async () => { if (++requests > 12) throw new ApiError('regression guard', 403); throw new TypeError('network interrupted'); },
    wait: (_delay, signal) => new Promise((_resolve, reject) => { seen.push(signal); signal.addEventListener('abort', () => reject(signal.reason), { once: true }); wake(true); }),
  }), /budget|repeatedly ended/);
  assert.equal(requests, 6, 'at most five automatic connections can start in sixty seconds');
  assert.equal(seen.length, 5);
});

test('a connection must remain stable before it replenishes consecutive retries', async () => {
  let now = 0, requests = 0;
  const delays = [];
  const abort = new AbortController();
  await assert.rejects(streamWorkspacePresentation({ ...base(), now: () => now, signal: abort.signal, retryLimit: 1,
    request: async () => {
      if (++requests === 4) { abort.abort(); throw new DOMException('Aborted', 'AbortError'); }
      return new Response(new ReadableStream({ start(writer) { setImmediate(() => { now += 30_001; writer.enqueue(encoder.encode(': heartbeat\n\n')); writer.close(); }); } }), { headers: { 'content-type': 'text/event-stream' } });
    }, wait: async delay => { delays.push(delay); now += delay; },
  }), { name: 'AbortError' });
  assert.equal(requests, 4);
  assert.deepEqual(delays, [500, 500, 500]);
});

test('offline pages wait for an online signal instead of attempting requests', async () => {
  let online = false, wake, requests = 0;
  const abort = new AbortController();
  const pending = streamWorkspacePresentation({ ...base(), signal: abort.signal, isOnline: () => online,
    subscribeWake: callback => { wake = callback; return () => {}; },
    request: async () => { requests++; abort.abort(); throw new DOMException('Aborted', 'AbortError'); },
  });
  const rejected = assert.rejects(pending, { name: 'AbortError' });
  await tick();
  assert.equal(requests, 0);
  wake(false); await tick();
  assert.equal(requests, 0, 'visibility alone does not turn an offline browser into a working network');
  online = true; wake(true);
  await rejected;
  assert.equal(requests, 1);
});

test('offline waits are cancellable without a surviving event listener', async () => {
  let unsubscribed = 0;
  const abort = new AbortController();
  const pending = streamWorkspacePresentation({ ...base(), signal: abort.signal, isOnline: () => false,
    subscribeWake: () => () => { unsubscribed++; }, request: async () => { throw new Error('offline request'); },
  });
  const rejected = assert.rejects(pending, { name: 'AbortError' });
  await tick(); abort.abort();
  await rejected;
  assert.equal(unsubscribed, 1);
});


test('exponential backoff uses jitter without changing its retry count', async () => {
  for (const [random, expected] of [[0, [250, 500]], [1, [750, 1500]]]) {
    const delays = [];
    let requests = 0;
    await assert.rejects(streamWorkspacePresentation({ ...base(), random: () => random, retryLimit: 2,
      request: async () => { requests++; return response(''); }, wait: async delay => { delays.push(delay); },
    }), /budget|repeatedly ended/);
    assert.equal(requests, 3);
    assert.deepEqual(delays, expected);
  }
});

test('Retry-After seconds delay recovery and wakeups cannot skip its minimum', async () => {
  let now = 0, wake, requests = 0;
  const delays = [];
  await assert.rejects(streamWorkspacePresentation({ ...base(), now: () => now, retryLimit: 1,
    subscribeWake: callback => { wake = callback; return () => {}; },
    request: async () => { if (++requests === 1) throw new ApiError('busy', 429, null, '4'); return response(''); },
    wait: (delay, signal) => { delays.push(delay); if (delays.length > 1) { now += delay; return Promise.resolve(); } return new Promise((_resolve, reject) => { signal.addEventListener('abort', () => reject(signal.reason), { once: true }); now += 500; wake(true); }); },
  }), /budget|repeatedly ended/);
  assert.equal(requests, 2);
  assert.deepEqual(delays, [4000, 3500]);
});

test('Retry-After HTTP dates are honored and excessive server delays stop automatic recovery', async () => {
  const epoch = Date.parse('2026-10-23T10:00:00Z');
  for (const [header, expectedRequests, expectedDelays] of [['Fri, 23 Oct 2026 10:00:06 GMT', 2, [6000]], ['3600', 1, []]]) {
    let requests = 0;
    const delays = [];
    await assert.rejects(streamWorkspacePresentation({ ...base(), now: () => epoch, retryLimit: 1,
      request: async () => { if (++requests === 1) throw new ApiError('busy', 503, null, header); return response(''); }, wait: async delay => { delays.push(delay); },
    }), /budget|repeatedly ended|busy/);
    assert.equal(requests, expectedRequests);
    assert.deepEqual(delays, expectedDelays);
  }
});


test('forced wakes on an active connection consume the same consecutive budget', async () => {
  let wake, requests = 0;
  await assert.rejects(streamWorkspacePresentation({ ...base(), retryLimit: 2,
    subscribeWake: callback => { wake = callback; return () => {}; },
    request: async (_path, options) => {
      if (++requests > 8) throw new ApiError('regression guard', 403);
      return new Response(new ReadableStream({ start() { setImmediate(() => { if (!options.signal.aborted) wake(true); }); } }), { headers: { 'content-type': 'text/event-stream' } });
    },
  }), /budget|repeatedly ended/);
  assert.equal(requests, 3);
});

test('permanent HTTP and protocol rejection never spend an automatic retry', async () => {
  for (const status of [400, 401, 403, 404]) {
    let requests = 0;
    await assert.rejects(streamWorkspacePresentation({ ...base(),
      request: async () => { requests++; throw new ApiError('permanent', status); },
      wait: async () => { assert.fail('a permanent failure cannot delay a recovery'); },
    }), /permanent/);
    assert.equal(requests, 1);
  }
  let requests = 0;
  await assert.rejects(streamWorkspacePresentation({ ...base(), request: async () => { requests++; return new Response('proxy error'); } }), /not an event stream/);
  assert.equal(requests, 1);
});

test('the HTTP client retains Retry-After without changing the existing error payload', async context => {
  configureApi({ apiBaseUrl: 'https://synthetic.invalid' });
  const body = { error: 'rate_limited' };
  context.mock.method(globalThis, 'fetch', async () => new Response(JSON.stringify(body), { status: 429, headers: { 'Content-Type': 'application/json', 'Retry-After': '7' } }));
  await assert.rejects(apiResponse('/api/synthetic'), error => {
    assert.ok(error instanceof ApiError);
    assert.equal(error.message, 'rate_limited');
    assert.equal(error.status, 429);
    assert.deepEqual(error.payload, body);
    assert.equal(error.retryAfter, '7');
    return true;
  });
});
