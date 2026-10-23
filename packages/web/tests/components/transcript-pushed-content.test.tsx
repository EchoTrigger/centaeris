// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, test, vi } from 'vitest';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController';
import { TranscriptBlockRow } from '../../src/chat/TranscriptBlockContent';
import { TranscriptReferencedContent } from '../../src/chat/TranscriptReferencedContent';

const { read } = vi.hoisted(() => ({ read: vi.fn() }));
vi.mock('../../src/i18n', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock('../../src/chat/MarkdownContent', () => ({ MarkdownContent: ({ text }: { text: string }) => <div>{text}</div> }));
vi.mock('../../src/chat/transcriptContentRanges', () => ({ loadTranscriptContentRange: read }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); read.mockReset(); });

test('evicted historical content waits until it becomes visible before demand reading', async () => {
  let visible: (() => void) | undefined;
  vi.stubGlobal('IntersectionObserver', class {
    constructor(callback: (entries: { isIntersecting: boolean }[]) => void) { visible = () => callback([{ isIntersecting: true }]); }
    observe() {}
    disconnect() {}
  });
  read.mockResolvedValue({ content: 'Visible historical content', startOffset: '0', endOffset: '26', hasMore: false });
  render(<TranscriptReferencedContent sessionId="session" projectionGeneration="generation"
    reference={{ refId: 'session-event:history', revision: '1', byteLength: '26' }} mode="markdown" deferUntilVisible />);
  await act(async () => {});
  expect(read).not.toHaveBeenCalled();
  await act(async () => visible?.());
  await waitFor(() => expect(screen.getByText('Visible historical content')).toBeTruthy());
  expect(read).toHaveBeenCalledTimes(1);
});


for (const deferredReturn of [false, true]) test(`async snapshot publication owns pushed references before React can start an HTTP reader (${deferredReturn ? 'deferred return' : 'immediate async return'})`, async () => {
  const store = createTranscriptViewStore();
  const common = { schema: 'workspace.presentation.v1', workspaceId: 'workspace', agentId: 'agent', sessionId: 'session' } as const;
  const reference = { refId: 'session-event:answer', revision: '1', byteLength: '6' };
  const page = { schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', sourceHighWater: '1',
    olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '1' }],
    blocks: [{ blockId: 'answer', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 0 }, body: { kind: 'assistantText',
      content: { inlineContent: null, sourceRef: reference }, status: 'completed' } }],
  } as const;
  const controller = new WorkspacePresentationController({ workspaceId: 'workspace', agentId: 'agent', sessionId: 'session', store,
    applySnapshot: async fresh => {
      const epoch = store.openTail(fresh);
      if (deferredReturn) await new Promise(resolve => setTimeout(resolve, 0));
      return epoch;
    },
  });
  read.mockResolvedValue({ content: 'abcdef', startOffset: '0', endOffset: '6', hasMore: false });
  render(<TranscriptBlockRow store={store} blockId="answer" />);
  await act(async () => controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'snapshot', page } }));
  expect(read).not.toHaveBeenCalled();
  await act(async () => controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'content', range: {
    schema: 'transcript.content.range.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation',
    ...reference, startOffset: '0', endOffset: '6', content: 'abcdef', hasMore: false,
  } } }));
  expect(screen.getByText('abcdef')).toBeTruthy();
  expect(read).not.toHaveBeenCalled();
});
