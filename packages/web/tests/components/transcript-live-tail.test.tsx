// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { useSyncExternalStore } from 'react';
import { configureApi } from '../../src/api';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController';
import { TranscriptToolOperationRegistry } from '../../src/chat/transcriptToolOperations';
import { TranscriptBlockRow, TranscriptLiveTail, TranscriptToolGroupCard } from '../../src/chat/TranscriptBlockContent';

vi.mock('../../src/i18n', () => ({ useTranslation: () => ({ t: (key: string) => key, i18n: { language: 'en' } }) }));
vi.mock('../../src/chat/MarkdownContent', () => ({ MarkdownContent: ({ text }: { text: string }) => <div data-testid="markdown-body">{text}</div> }));
vi.mock('../../src/chat/useStickToBottom', () => ({ useStickToBottom: () => ({ current: null }) }));
vi.mock('../../src/chat/AnimatedDisclosure', () => ({ AnimatedDisclosure: ({ expanded, children }: { expanded: boolean; children: React.ReactNode }) => expanded ? children : null }));
afterEach(cleanup);

const live = { messageId: 'answer', turnId: 'turn', afterSourceHighWater: '0', revision: 1,
  text: 'First streamed answer', reasoning: null };

test('ordinary streamed answer is visible even when the model has no reasoning', () => {
  const view = render(<TranscriptLiveTail live={live} />);
  expect(screen.getByText('First streamed answer')).toBeTruthy();
  view.rerender(<TranscriptLiveTail live={{ ...live, revision: 2, text: 'First streamed answer continues' }} />);
  expect(screen.getByText('First streamed answer continues')).toBeTruthy();
  expect(screen.queryByText('First streamed answer')).toBeNull();
});

test('answer streams beside the existing manually controlled reasoning disclosure', async () => {
  const user = userEvent.setup();
  const reasoning = { blockId: 'reasoning:request', requestId: 'request', text: 'Reasoning in progress' };
  render(<TranscriptLiveTail live={{ ...live, reasoning }} />);
  const disclosure = screen.getByRole('button', { name: 'reasoningBlock.thinking' });
  expect(disclosure.getAttribute('aria-expanded')).toBe('false');
  expect(screen.getByText('First streamed answer')).toBeTruthy();
  expect(screen.queryByRole('region')).toBeNull();
  await user.click(disclosure);
  expect(disclosure.getAttribute('aria-expanded')).toBe('true');
  expect(screen.getByRole('region').textContent).toContain('Reasoning in progress');
  expect(screen.getByText('First streamed answer')).toBeTruthy();
});

test('live answer can occupy the persistent response area while reasoning keeps its disclosure', () => {
  const reasoning = { blockId: 'reasoning:request', requestId: 'request', text: 'Reasoning in progress' };
  const view = render(<TranscriptLiveTail live={{ ...live, reasoning }} mode="text" />);
  expect(screen.getByText('First streamed answer')).toBeTruthy();
  expect(screen.queryByRole('button')).toBeNull();
  view.rerender(<TranscriptLiveTail live={{ ...live, reasoning }} mode="reasoning" />);
  expect(screen.queryByText('First streamed answer')).toBeNull();
  expect(screen.getByRole('button', { name: 'reasoningBlock.thinking' })).toBeTruthy();
});


test('live text hands off once to referenced committed content even when its checkpoint is lost', async () => {
  const store = createTranscriptViewStore();
  const identity = { workspaceId: 'workspace', agentId: 'agent', sessionId: 'session' };
  const controller = new WorkspacePresentationController({ ...identity, store });
  const common = { schema: 'workspace.presentation.v1', ...identity } as const;
  const page = { schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation',
    sourceHighWater: '0', olderCursor: null, hasOlder: false, resumeCursors: [], blocks: [] } as const;
  await controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'snapshot', page } });
  await controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'live', agentRunId: 'run',
    live: { messageId: 'answer', turnId: 'turn', afterSequence: 0, revision: 1, text: 'abcdef', reasoning: null } } });
  function View() {
    const current = useSyncExternalStore(store.subscribeLive, store.getLiveSnapshot, store.getLiveSnapshot);
    return <><TranscriptBlockRow store={store} blockId="answer" />{current ? <TranscriptLiveTail live={current} mode="text" /> : null}</>;
  }
  render(<View />);
  const once = () => {
    expect(screen.getAllByTestId('markdown-body').map(element => element.textContent)).toEqual(['abcdef']);
  };
  once();
  const reference = { refId: 'session-event:answer', revision: '1', byteLength: '6' };
  const block = { blockId: 'answer', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 0 },
    body: { kind: 'assistantText', content: { inlineContent: null, sourceRef: reference }, status: 'completed' } };
  await act(async () => controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'patches', page: {
    schema: 'transcript.patch.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation',
    throughSourceHighWater: '1', nextSourceHighWater: '1', hasMore: false, patches: [{ schema: 'transcript.patch.v1', sessionId: 'session',
      projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', sourceHighWater: '1', streamId: 'workspace-transcript.v1',
      appliedCursor: '1', upserts: [block], removals: [] }],
  } } }));
  once();
  const content = (startOffset: string, endOffset: string, text: string, hasMore: boolean) => ({ cursor: null, item: { ...common, kind: 'content' as const,
    range: { schema: 'transcript.content.range.v1' as const, sessionId: 'session', projectionVersion: 'transcript.projection.v1' as const,
      projectionGeneration: 'generation', ...reference, startOffset, endOffset, content: text, hasMore } } });
  await act(async () => controller.acceptWithBackpressure(content('0', '3', 'abc', true)));
  once();
  await act(async () => controller.acceptWithBackpressure(content('3', '6', 'def', false)));
  once();
  expect(controller.lastCursor).toBeNull();
});


test('only explicitly truncated tool detail gains an authorized complete-download link', async () => {
  configureApi({ apiBaseUrl: 'https://example.test' });
  const user = userEvent.setup();
  const store = createTranscriptViewStore();
  store.openTail({ schema: 'transcript.page.v1', sessionId: 'session/one', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation',
    sourceHighWater: '1', olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '1' }],
    blocks: [{ blockId: 'tool', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 0 }, body: {
      kind: 'tool', callId: 'call', toolName: 'exec_command', status: 'completed', summary: 'Inspect files', summaryRef: null, outputRef: null,
    } }],
  });
  const registry = new TranscriptToolOperationRegistry();
  const record = { callId: 'call', call: { normalizedInput: { command: 'pwd' } }, result: null, operations: [], status: 'completed' as const,
    previewTruncated: true, detailSourceEventId: 'event:large/input' };
  vi.spyOn(registry, 'getSnapshot').mockReturnValue({ version: 1, records: new Map([['call', record]]) });
  const view = render(<TranscriptToolGroupCard store={store} blockIds={['tool']} toolOperations={registry} />);
  await user.click(screen.getByRole('button', { expanded: false }));
  const link = screen.getByRole('link', { name: 'libraryObjectRoute.downloadFile' });
  expect(link.getAttribute('href')).toBe('https://example.test/api/sessions/session%2Fone/presentation/operation?sourceEventId=event%3Alarge%2Finput');
  expect(link.hasAttribute('download')).toBe(true);
  vi.mocked(registry.getSnapshot).mockReturnValue({ version: 2, records: new Map([['call', { ...record, previewTruncated: false }]]) });
  view.rerender(<TranscriptToolGroupCard store={store} blockIds={['tool']} toolOperations={registry} />);
  expect(screen.queryByRole('link', { name: 'libraryObjectRoute.downloadFile' })).toBeNull();
});
