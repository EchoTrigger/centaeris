// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, test, vi } from 'vitest';
import { ConversationReadFeedback } from '../../src/chat/ConversationReadFeedback';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore';
vi.mock('../../src/i18n', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
afterEach(cleanup);
const reference = { refId: 'session-event:answer', revision: '1', byteLength: '8' };
const page = { schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
  projectionGeneration: 'generation', sourceHighWater: '1', olderCursor: null, hasOlder: false,
  resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '1' }], blocks: [{ blockId: 'answer', blockRevision: '1',
    orderKey: { sourceSequence: '1', ordinal: 0 }, body: { kind: 'assistantText', status: 'completed', content: { inlineContent: null, sourceRef: reference } } }] };

test('snapshot structure without its body fails as first load, while a readable pushed prefix can keep reading', () => {
  const store = createTranscriptViewStore();
  const epoch = store.openTail(page);
  store.reservePresentationContent(epoch, page.blocks);
  render(<ConversationReadFeedback store={store} placement="content" error="" paused ready={false} loading={false}
    checking={false} reconnecting={false} onCheck={() => {}} onRetry={() => {}} />);
  expect(screen.getByText('appRoute.initialHistoryFailed')).toBeTruthy();
  expect(screen.queryByText('appRoute.streamPaused')).toBeNull();
  act(() => { store.applyPresentationContentRange({ schema: 'transcript.content.range.v1', sessionId: 'session',
    projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', ...reference,
    startOffset: '0', endOffset: '4', content: 'read', hasMore: true }, epoch); });
  expect(screen.getByText('appRoute.streamPaused')).toBeTruthy();
  expect(store.presentationContentReady()).toBe(false);
});

test('loading and an authoritative empty view have different messages', () => {
  const store = createTranscriptViewStore();
  store.openTail({ ...page, blocks: [] });
  const props = { store, placement: 'empty', error: '', paused: false, ready: false, loading: false,
    checking: false, reconnecting: false, onCheck() {}, onRetry() {} };
  const view = render(<ConversationReadFeedback {...props} />);
  expect(screen.getByText('appRoute.loadingConversation')).toBeTruthy();
  view.rerender(<ConversationReadFeedback {...props} ready />);
  expect(screen.getByText('appRoute.emptyConversation')).toBeTruthy();
});
