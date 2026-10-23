// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { URL as FileURL } from 'node:url';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, expect, test, vi } from 'vitest';
import { MarkdownContent, StreamingMarkdownContent } from '../../src/chat/MarkdownContent';
import { TranscriptBlockRow, TranscriptLiveTail } from '../../src/chat/TranscriptBlockContent';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController';

vi.mock('../../src/i18n', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
const cases = JSON.parse(readFileSync(new FileURL('../../../core/tests/fixtures/visualizations.json', import.meta.url), 'utf8'));
const fence = '`'.repeat(3);
const source = `Before\n\n${fence}centaeris_visual\n${cases[0].source}\n${fence}\n\nAfter`;
const pending = source.slice(0, source.lastIndexOf(fence));
afterEach(cleanup);

test('assistant visuals preserve surrounding Web Markdown and use the accepted ordered presentation', async () => {
  const view = render(<MarkdownContent text={source} allowVisuals />);
  const sequence = await screen.findByRole('list', {}, { timeout: 10000 });
  expect(within(sequence).getByText('Run A')).toBeTruthy();
  expect(within(sequence).getByText('Run B')).toBeTruthy();
  expect(screen.getByText('Before')).toBeTruthy();
  expect(screen.getByText('After')).toBeTruthy();
  expect(view.container.textContent).not.toContain('"elements"');
  expect(view.container.querySelector('[data-slot="card"]')).toBeTruthy();
  view.rerender(<MarkdownContent text={source + ' More prose'} allowVisuals />);
  expect(screen.getByRole('list')).toBe(sequence);
  expect(screen.getByText('After More prose')).toBeTruthy();
});

test('streaming incomplete envelopes wait privately, then render once without losing surrounding prose', async () => {
  const view = render(<StreamingMarkdownContent text={pending} />);
  expect(screen.getByRole('status').getAttribute('aria-busy')).toBe('true');
  expect(view.container.textContent).not.toContain('elements');
  expect(screen.queryByText('Original source')).toBeNull();
  view.rerender(<StreamingMarkdownContent text={source} finalized />);
  const sequence = await screen.findByRole('list', {}, { timeout: 10000 });
  view.rerender(<StreamingMarkdownContent text={source + '\n\nMore prose'} finalized />);
  expect(screen.getByRole('list')).toBe(sequence);
  expect(screen.getByText('More prose')).toBeTruthy();
});

test('bad or interrupted visuals expose escaped original source while ordinary Markdown stays unchanged', () => {
  const invalid = `${fence}centaeris_visual\n{"schema":"unsupported","html":"<script>bad()</script>"}\n${fence}`;
  const view = render(<MarkdownContent text={'Before\n\n' + invalid + '\n\nAfter'} allowVisuals />);
  expect(screen.getByText('Original source')).toBeTruthy();
  expect(view.container.textContent).toContain('<script>bad()</script>');
  expect(view.container.querySelector('script')).toBeNull();
  view.rerender(<MarkdownContent text={pending} allowVisuals />);
  expect(screen.getByText('Incomplete visualization')).toBeTruthy();
  expect(screen.getByText('Original source')).toBeTruthy();
  view.rerender(<MarkdownContent text={'# Heading\n\n**Bold** and `code`\n\n[Unsafe](javascript:bad())\n\n- One\n- Two\n\n' + source} />);
  expect(screen.getByRole('heading', { name: 'Heading' })).toBeTruthy();
  expect(screen.getByText('Bold').tagName).toBe('STRONG');
  expect(screen.queryByRole('link', { name: 'Unsafe' })).toBeNull();
  expect(view.container.textContent).toContain(cases[0].source);
});

test('quoted and list examples are not executed as assistant visuals', () => {
  const view = render(<MarkdownContent text={source.split('\n').map(line => '> ' + line).join('\n')} allowVisuals />);
  expect(screen.queryByText('Loading visualization…')).toBeNull();
  expect(view.container.textContent).toContain('"elements"');
});

test('live assistant text and committed pushed content share visual streaming completion, without changing reasoning disclosure', async () => {
  const live = { afterSequence: '0', revision: '1', turnId: 'turn', messageId: 'answer', text: pending };
  const view = render(<TranscriptLiveTail live={live} mode="text" />);
  expect(screen.getByRole('status').textContent).toBe('Loading visualization…');
  expect(view.container.textContent).not.toContain('elements');
  view.rerender(<TranscriptLiveTail live={{ ...live, text: source }} mode="text" />);
  expect(await screen.findByRole('list', {}, { timeout: 10000 })).toBeTruthy();
  view.unmount();
  const store = createTranscriptViewStore();
  const common = { schema: 'workspace.presentation.v1', workspaceId: 'workspace', agentId: 'agent', sessionId: 'session' } as const;
  const byteLength = String(new TextEncoder().encode(source).length);
  const reference = { refId: 'session-event:answer', revision: '1', byteLength };
  const controller = new WorkspacePresentationController({ ...common, store });
  const page = { schema: 'transcript.page.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', sourceHighWater: '1',
    olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '1' }],
    blocks: [{ blockId: 'answer', blockRevision: '1', orderKey: { sourceSequence: '1', ordinal: 0 }, body: { kind: 'assistantText',
      content: { inlineContent: null, sourceRef: reference }, status: 'completed' } }],
  } as const;
  await act(async () => controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'snapshot', page } }));
  const row = render(<TranscriptBlockRow store={store} blockId="answer" />);
  const split = new TextEncoder().encode(pending).length;
  const range = { schema: 'transcript.content.range.v1', sessionId: 'session', projectionVersion: 'transcript.projection.v1', projectionGeneration: 'generation', ...reference } as const;
  await act(async () => controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'content', range: { ...range, startOffset: '0', endOffset: String(split), content: pending, hasMore: true } } }));
  expect(screen.getByRole('status').textContent).toBe('Loading visualization…');
  expect(row.container.textContent).not.toContain('elements');
  await act(async () => controller.acceptWithBackpressure({ cursor: null, item: { ...common, kind: 'content', range: { ...range, startOffset: String(split), endOffset: byteLength, content: source.slice(pending.length), hasMore: false } } }));
  expect(await screen.findByRole('list', {}, { timeout: 10000 })).toBeTruthy();
});
