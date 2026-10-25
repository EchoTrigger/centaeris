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
