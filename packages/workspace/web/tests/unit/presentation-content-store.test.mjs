import assert from 'node:assert/strict';
import test from 'node:test';
import { createPresentationContentStore, PRESENTATION_CONTENT_CACHE_BYTES, PRESENTATION_CONTENT_PREVIEW_BYTES } from '../../src/chat/presentationContentStore.ts';
const identity = { sessionId: 'session', projectionGeneration: 'generation' };
function range(reference, start, content, hasMore) {
  return { schema: 'transcript.content.range.v1', ...identity, projectionVersion: 'transcript.projection.v1', ...reference,
    startOffset: String(start), endOffset: String(start + new TextEncoder().encode(content).byteLength), content, hasMore };
}
function fill(store, reference, end) {
  let start = 0;
  while (start < end) {
    const length = Math.min(65536, end - start);
    store.apply(range(reference, start, 'a'.repeat(length), start + length < Number(reference.byteLength)), identity);
    start += length;
  }
}

test('large normal body has a bounded explicit preview and reaches a checkpoint-ready state', () => {
  const store = createPresentationContentStore();
  const reference = { refId: 'session-event:large', revision: '1', byteLength: String(PRESENTATION_CONTENT_PREVIEW_BYTES + 30) };
  store.reserve([reference]);
  fill(store, reference, PRESENTATION_CONTENT_PREVIEW_BYTES);
  assert.equal(store.ready(), true);
  assert.equal(store.get(reference).limited, true);
  assert.equal(store.get(reference).content.length, PRESENTATION_CONTENT_PREVIEW_BYTES);
  assert.equal(store.bytes(), PRESENTATION_CONTENT_PREVIEW_BYTES);
  assert.throws(() => store.apply(range(reference, PRESENTATION_CONTENT_PREVIEW_BYTES, 'a', true), identity), /budget/);
});

test('UTF-8 preview may finish at the final complete code point before its byte cap', () => {
  const store = createPresentationContentStore();
  const reference = { refId: 'session-event:unicode-large', revision: '1', byteLength: String(PRESENTATION_CONTENT_PREVIEW_BYTES + 30) };
  store.reserve([reference]);
  fill(store, reference, PRESENTATION_CONTENT_PREVIEW_BYTES - 2);
  assert.equal(store.ready(), true);
  assert.equal(store.get(reference).limited, true);
  assert.equal(store.get(reference).content.length, PRESENTATION_CONTENT_PREVIEW_BYTES - 2);
});

test('multiple large refs preserve lightweight completion receipts while resident content stays bounded', () => {
  const store = createPresentationContentStore();
  const references = ['first', 'second', 'third'].map(id => ({ refId: `session-event:${id}`, revision: '1', byteLength: String(8 * 1024 * 1024) }));
  store.reserve(references);
  for (const reference of references) {
    fill(store, reference, Number(reference.byteLength));
    assert.ok(store.bytes() <= PRESENTATION_CONTENT_CACHE_BYTES);
  }
  assert.equal(store.ready(), true, 'eviction must not force an already applied page to reconnect forever');
  assert.equal(store.get(references[0]), null);
  assert.equal(store.get(references[2]).content.length, 8 * 1024 * 1024);
  store.retain([references[2]]);
  assert.equal(store.bytes(), 8 * 1024 * 1024);
  store.clear();
  assert.equal(store.bytes(), 0);
});

test('content belongs to a selected immutable reference and cannot append gaps or another generation', () => {
  const store = createPresentationContentStore();
  const reference = { refId: 'session-event:answer', revision: '1', byteLength: '6' };
  store.reserve([reference]);
  assert.throws(() => store.apply(range(reference, 3, 'abc', false), identity), /continuation/);
  assert.throws(() => store.apply({ ...range(reference, 0, 'abc', true), projectionGeneration: 'old' }, identity));
  assert.equal(store.get(reference).content, '');
  assert.equal(store.ready(), false);
});

test('readiness follows unique reservations, completion, retention, and reset before notification', () => {
  const store = createPresentationContentStore();
  const empty = { refId: 'session-event:empty', revision: '1', byteLength: '0' };
  const first = { refId: 'session-event:first', revision: '1', byteLength: '3' };
  const pending = { refId: 'session-event:pending', revision: '1', byteLength: '1' };
  const notifications = [];
  store.subscribe(() => notifications.push([store.getVersion(), store.ready()]));
  assert.equal(store.ready(), true);
  store.reserve([empty, first, first]);
  assert.equal(store.ready(), false);
  const page = range(first, 0, 'abc', false);
  store.apply(page, identity, () => {
    assert.equal(store.ready(), true);
    assert.equal(store.getVersion(), 0);
  });
  store.apply(page, identity, () => assert.fail('replay must not notify'));
  assert.deepEqual(notifications, [[1, true]]);
  store.reserve([first, pending]);
  assert.equal(store.ready(), false);
  store.retain([empty, first]);
  assert.equal(store.ready(), true);
  assert.equal(store.bytes(), 3);
  store.reset([pending, pending]);
  assert.equal(store.ready(), false);
  assert.equal(store.bytes(), 0);
  assert.equal(store.get(first), null);
  store.clear();
  assert.deepEqual(notifications, [[1, true], [2, false], [3, true]]);
});

test('validated Unicode byte ranges account for resident bytes and reject mismatched content', () => {
  const store = createPresentationContentStore();
  const reference = { refId: 'session-event:unicode', revision: '1', byteLength: '7' };
  store.reserve([reference]);
  const page = range(reference, 0, '界🙂', false);
  assert.throws(() => store.apply({ ...page, endOffset: '6' }, identity), /invalid transcript content range/);
  assert.equal(store.bytes(), 0);
  assert.equal(store.ready(), false);
  store.apply(page, identity);
  assert.equal(store.bytes(), 7);
  assert.equal(store.get(reference).content, '界🙂');
  assert.equal(store.ready(), true);
});

test('new content touches resident order, replay does not, and evicted pending content can finish', () => {
  const store = createPresentationContentStore();
  const size = 8 * 1024 * 1024;
  const first = { refId: 'session-event:first', revision: '1', byteLength: String(size) };
  const second = { refId: 'session-event:second', revision: '1', byteLength: String(size + 1) };
  const third = { refId: 'session-event:third', revision: '1', byteLength: String(size) };
  store.reserve([first, second, third]);
  fill(store, first, size / 2);
  fill(store, second, size);
  for (let start = size / 2; start < size; start += 65536) {
    store.apply(range(first, start, 'a'.repeat(65536), start + 65536 < size), identity);
  }
  const replay = range(second, 0, 'a'.repeat(65536), true);
  const version = store.getVersion();
  store.apply(replay, identity);
  assert.equal(store.getVersion(), version);
  fill(store, third, size);
  assert.equal(store.get(first).content.length, size);
  assert.equal(store.get(second), null);
  assert.equal(store.isEvicted(second), true);
  assert.equal(store.bytes(), PRESENTATION_CONTENT_CACHE_BYTES);
  assert.equal(store.ready(), false);
  store.apply(range(second, size, 'b', false), identity);
  assert.equal(store.ready(), true);
  assert.equal(store.isReady(second), true);
  assert.equal(store.get(second), null);
  assert.equal(store.bytes(), PRESENTATION_CONTENT_CACHE_BYTES);
  store.apply(replay, identity);
  assert.throws(() => store.apply({ ...replay, content: 'z'.repeat(65536) }, identity), /conflicting/);
  store.retain([third]);
  assert.equal(store.bytes(), size);
  store.reserve([first]);
  fill(store, first, size);
  assert.equal(store.get(third).content.length, size);
  assert.equal(store.get(first).content.length, size);
  assert.equal(store.bytes(), PRESENTATION_CONTENT_CACHE_BYTES);
});
