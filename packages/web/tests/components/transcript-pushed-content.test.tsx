// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController';
import { TranscriptBlockRow, TranscriptToolGroupCard } from '../../src/chat/TranscriptBlockContent';
import { TranscriptToolOperationRegistry } from '../../src/chat/transcriptToolOperations';
import { TranscriptReferencedContent } from '../../src/chat/TranscriptReferencedContent';

const { read } = vi.hoisted(() => ({ read: vi.fn() }));
vi.mock('../../src/i18n', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock('../../src/chat/MarkdownContent', () => ({ MarkdownContent: ({ text }: { text: string }) => <div>{text}</div> }));
vi.mock('../../src/chat/transcriptContentRanges', () => ({ loadTranscriptContentRange: read }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); read.mockReset(); });

for (const delivery of ['content', 'execution_file']) test(`material ${delivery} preserves its output and only content reads expose recorded evidence`, async () => {
  vi.stubGlobal('matchMedia', () => ({ matches: true }));
  const user = userEvent.setup();
  const store = createTranscriptViewStore();
  const toolOperations = new TranscriptToolOperationRegistry();
  const result = delivery === 'execution_file'
    ? { kind: 'execution_file', delivery: 'execution_file', inputRef: 'library:report', path: '/workspace/inputs/report.zip',
      displayName: 'Report archive', contentType: 'application/zip', sourceVersion: '3', sha256: 'sha256:' + 'a'.repeat(64), sizeBytes: 1024 }
    : { disposition: 'ready', items: [{ displayName: 'Report', content: 'Report evidence' }] };
  const output = JSON.stringify(result);
  const byteLength = String(new TextEncoder().encode(output).length);
  const reference = { refId: 'session-event:material-output', revision: '1', byteLength };
  store.openTail({ schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1',
    projectionGeneration: 'generation', sourceHighWater: '3', olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '3' }],
    blocks: [{ blockId: 'material', blockRevision: '1', orderKey: { sourceSequence: '3', ordinal: 0 }, body: {
      kind: 'tool', callId: 'call', toolName: 'read_material', status: 'completed', summary: 'read_material', summaryRef: null, outputRef: reference,
    } }],
  });
  toolOperations.applyPresentationDetail({ kind: 'call', callId: 'call', toolName: 'read_material',
    normalizedInput: { input_ref: 'library:report', delivery }, resultState: null, operations: [],
    previewTruncated: false, sourceEventId: 'call-event' });
  read.mockResolvedValue({ content: output, startOffset: '0', endOffset: byteLength, hasMore: false });
  const citation = { citationId: 'citation:report', inputRef: 'library:report', displayName: 'Report', sourceToolCallId: 'call', sourceUrl: '/api/citations/citation:report' };
  const onShowCitation = vi.fn();
  render(<TranscriptToolGroupCard store={store} blockIds={['material']} toolOperations={toolOperations}
    citations={new Map(delivery === 'content' ? [['3', [citation]]] : [])} onShowCitation={onShowCitation} />);
  const heading = delivery === 'execution_file' ? /materialTool.executionFileReady/ : /toolGroup.read.completed/;
  await user.click(screen.getByRole('button', { name: heading }));
  await waitFor(() => expect(screen.getByText(output)).toBeTruthy());
  if (delivery === 'content') {
    await user.click(screen.getByRole('button', { name: 'Report' }));
    expect(onShowCitation).toHaveBeenCalledWith(citation, expect.anything());
  } else {
    expect(screen.queryByText('transcriptBlockContent.toolSources')).toBeNull();
    expect(screen.getByText(output).textContent).toContain('/workspace/inputs/report.zip');
    expect(screen.getByText(output).textContent).toContain('Report archive');
    await act(async () => toolOperations.applyPresentationDetail({ kind: 'result', callId: 'call', toolName: 'read_material',
      normalizedInput: null, resultState: 'failed', operations: [], previewTruncated: false, sourceEventId: 'failed-result' }));
    expect(screen.queryByRole('button', { name: /materialTool.executionFileReady/ })).toBeNull();
    expect(screen.getByRole('button', { name: /materialTool.executionFileAction/ })).toBeTruthy();
  }
});

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
  const common = { schema: 'workspace.presentation.v1', workspaceId: 'workspace', sessionId: 'session' } as const;
  const reference = { refId: 'session-event:answer', revision: '1', byteLength: '6' };
  const page = { schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', sourceHighWater: '1',
    olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '1' }],
    blocks: [{ blockId: 'answer', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 0 }, body: { kind: 'assistantText',
      content: { inlineContent: null, sourceRef: reference }, status: 'completed' } }],
  } as const;
  const controller = new WorkspacePresentationController({ workspaceId: 'workspace', sessionId: 'session', store,
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
