import assert from 'node:assert/strict';
import test from 'node:test';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore.ts';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController.ts';
import { createSessionViewCache } from '../../src/chat/sessionViewCache.ts';
import { createSessionNavigationStore } from '../../src/shell/sessionNavigationStore.ts';

const identity = { workspaceId: 'workspace', sessionId: 'session' };
const reference = { refId: 'session-event:answer', revision: '1', byteLength: '4' };
const block = (id: string, sequence: number, sourceRef = null as null | typeof reference) => ({
  blockId: id, blockRevision: '1', orderKey: { sourceSequence: String(sequence), ordinal: 0 },
  body: { kind: 'assistantText', status: 'completed', content: { inlineContent: sourceRef ? null : id, sourceRef } },
});
const page = { schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
  projectionGeneration: 'generation', sourceHighWater: '10', blocks: [block('answer', 10, reference)],
  olderCursor: 'older', hasOlder: true, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '10' }] };
const row = { id: 'session', workspaceId: 'workspace', title: 'Title', latestAttentionSequence: '0',
  readThroughSequence: '0', attentionRevision: '0', isManuallyUnread: false, isUnread: false };

test('only an applied checkpoint can cache a view with pushed content, history and its resume cursor', async () => {
  const store = createTranscriptViewStore();
  const controller = new WorkspacePresentationController({ ...identity, store });
  const send = (kind: string, body: object, cursor: string | null = null) => controller.acceptWithBackpressure({ cursor,
    item: { schema: 'workspace.presentation.v1', ...identity, kind, ...body } } as never);
  await send('snapshot', { page });
  assert.equal(controller.captureAppliedView(), null, 'snapshot headers and blocks are not a ready cached view');
  await send('content', { range: { schema: 'transcript.content.range.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
    projectionGeneration: 'generation', ...reference, startOffset: '0', endOffset: '4', content: 'body', hasMore: false } });
  assert.equal(controller.captureAppliedView(), null, 'complete bytes still require their applied checkpoint');
  await send('checkpoint', { projectionGeneration: 'generation', sourceHighWater: '10', agentRun: null }, 'checkpoint:10');
  store.prependPage({ ...page, blocks: [block('history', 1)], olderCursor: null, hasOlder: false }, store.getListSnapshot().viewEpoch);
  const cached = controller.captureAppliedView()!;
  assert.equal(cached.cursor, 'checkpoint:10');
  const oldEpoch = store.getListSnapshot().viewEpoch;
  store.clear(); cached.restore();
  assert.deepEqual(store.getListSnapshot().blockIds, ['history', 'answer']);
  assert.equal(store.getListSnapshot().hasOlder, false);
  assert.equal(store.getListSnapshot().projectionGeneration, 'generation');
  assert.equal(store.getPresentationContentSnapshot(reference)?.content, 'body');
  assert.equal(store.presentationContentReady(), true);
  assert.ok(store.getListSnapshot().viewEpoch > oldEpoch, 'restoring invalidates late readers from the previous view');
  assert.equal(store.prependPage({ ...page, blocks: [], olderCursor: null, hasOlder: false }, oldEpoch), false);
  const resumed = new WorkspacePresentationController({ ...identity, store, cursor: cached.cursor });
  assert.equal(resumed.lastCursor, 'checkpoint:10');
  assert.equal(resumed.captureAppliedView(), null, 'a cached view does not count as newly synchronized');
});

test('the twelve-view LRU touches hits and discards entries on authority clear', () => {
  const cache = createSessionViewCache();
  const entry = { cursor: 'cursor', restore() {} };
  for (let id = 0; id < 12; id++) cache.set(String(id), entry);
  cache.get('0'); cache.set('12', entry);
  assert.equal(cache.get('1'), undefined);
  assert.equal(cache.get('0'), entry);
  cache.clear(); assert.equal(cache.get('0'), undefined);
});

test('equal navigation snapshots retain records and arrays without publishing; attention conflicts still reject', () => {
  const store = createSessionNavigationStore();
  let publications = 0; store.subscribe(() => publications++);
  store.setSessions([row]); store.setProjects([{ id: 'project', name: 'Project' }]);
  const previous = store.getSnapshot();
  store.setSessions([{ ...row }]); store.setProjects([{ id: 'project', name: 'Project' }]);
  assert.equal(store.getSnapshot(), previous);
  assert.equal(publications, 2);
  store.setSessions([{ ...row, title: 'Renamed' }]);
  assert.equal(store.getSnapshot().sessions[0].title, 'Renamed');
  assert.throws(() => store.setSessions([{ ...row, isManuallyUnread: true, isUnread: true }]), /conflict/);
});
