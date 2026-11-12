import { createRoot } from 'react-dom/client';
import { createMemoryRouter, RouterProvider } from 'react-router';
import { FrontendProvider } from '@centaeris/ui/provider';
import { configureApi } from '../../src/api';
import { i18n } from '../../src/i18n';
import WorkspaceChatLayout from '../../src/routes/WorkspaceHomeRoute';
import { RecentlyDeleted } from '../../src/routes/RecentlyDeleted';
import WorkPrototypeRoute from '../../src/work/WorkPrototypeRoute';
import { WorkPrototypeProvider } from '../../src/work/workPrototype';
import SettingsRoute from '../../src/routes/SettingsRoute';
import '../../src/globals.css';

// Actual route, sidebar, transcript and transport; only the HTTP server is synthetic.
configureApi({ apiBaseUrl: location.origin });
document.documentElement.dataset.theme = new URLSearchParams(location.search).get('theme') ?? 'light';
await i18n.changeLanguage('en');
const workspace = { id: 'workspace', name: 'Navigation fixture', role: 'owner', avatarUrl: null };
const user = { id: 'user', email: 'synthetic@example.invalid' };
const rows = Array.from({ length: 48 }, (_, index) => ({ id: `session-${index}`, workspaceId: workspace.id,
  projectId: index < 4 ? 'project' : null, currentEnvironmentId: null, executionMode: null, title: index === 5 ? `Conversation 5 ${"long title ".repeat(20)}`.trim() : `Conversation ${index}`,
  origin: 'user', status: 'active', deletedAt: null, isPinned: false, isUnread: false, hasActiveAgentRun: false,
  updatedAt: '2026-11-07T00:00:00Z', latestAttentionSequence: '0', readThroughSequence: '0', attentionRevision: '0', isManuallyUnread: false }));
const requests: string[] = [], failures: string[] = [], mutations: string[] = [];
const streams = new Map<string, ReadableStreamDefaultController<Uint8Array>>();
const encoder = new TextEncoder();
window.addEventListener('error', event => failures.push(event.message));
window.addEventListener('unhandledrejection', event => failures.push(String(event.reason)));
window.fetch = async (input, options) => {
  const url = new URL(String(input), location.href);
  requests.push(url.pathname);
  if (options?.method && options.method !== "GET") mutations.push(options.method + " " + url.pathname);
  if (url.pathname.endsWith('/presentation/events')) {
    const id = url.searchParams.get('sessionId') ?? '';
    let closed = false;
    let owner: ReadableStreamDefaultController<Uint8Array>;
    return new Response(new ReadableStream({ start(controller) {
      owner = controller;
      streams.set(id, controller);
      options?.signal?.addEventListener('abort', () => {
        if (closed) return;
        closed = true;
        if (streams.get(id) === controller) streams.delete(id);
        controller.close();
      }, { once: true });
    }, cancel() {
      closed = true;
      if (streams.get(id) === owner) streams.delete(id);
    } }), { headers: { 'Content-Type': 'text/event-stream' } });
  }
  if (url.pathname === '/api/models') return Response.json({ models: [] });
  if (url.pathname.endsWith('/session-projects')) return Response.json({ projects: [{ id: 'project', name: 'Persistent group' }] });
  if (url.pathname === '/api/workspaces/workspace/sessions') return Response.json({ sessions: rows });
  if (url.pathname.endsWith('/assets')) return Response.json({ assets: [] });
  if (url.pathname.endsWith('/active-agent-run')) return Response.json({ schema: 'workspace.transcript.active_agent_run.v1', sessionId: url.pathname.split('/')[3], agentRun: null });
  if (url.pathname.endsWith('/turn-metadata')) return Response.json({ turns: [] });
  if (url.pathname.endsWith('/context-usage')) return Response.json({ schema: 'session.context_usage.v1', sessionId: url.pathname.split('/')[3], contextUsage: null });
  if (url.pathname === '/api/workspaces/workspace/trash') return Response.json({ items: [{ id: 'material', title: 'Deleted material',
    deletedAt: '2026-11-06T00:00:00Z', location: { label: 'Personal Library' }, deletedBy: { email: user.email } }],
    hasMore: false, nextCursor: null, filterOptions: { locations: [], deletedBy: [] } });
  const session = rows.find(row => url.pathname === `/api/sessions/${row.id}`);
  if (session) return Response.json({ session });
  throw new Error(`Unexpected continuity request: ${url.pathname}`);
};
const router = createMemoryRouter([{ id: 'authenticated', loader: () => ({ user }), children: [{ id: 'workspace', path: '/w/:workspaceId',
  loader: ({ params }) => ({ workspace, workspaces: [workspace], selectedSession: rows.find(row => row.id === params.sessionId) ?? null }),
  shouldRevalidate: () => true, Component: () => <WorkPrototypeProvider><WorkspaceChatLayout /></WorkPrototypeProvider>,
  children: [{ path: 'work/*', element: <WorkPrototypeRoute /> }, { path: 'sessions/:sessionId', element: null }, { path: 'app', element: null },
    { path: 'library', element: <h1>Navigation library</h1> },
    { path: 'settings/preferences', Component: SettingsRoute }, { path: 'reports', element: <main data-report-page><input aria-label="Report filter" defaultValue="Keep this filter" /></main> },
    { path: 'settings/archive', element: <RecentlyDeleted workspace={workspace} /> }],
}] }], { initialEntries: ['/w/workspace/sessions/session-4'] });

function send(id: string, kind: string, body: object, cursor?: string) {
  const stream = streams.get(id);
  if (!stream) throw new Error(`No active presentation for ${id}`);
  stream.enqueue(encoder.encode(`${cursor ? `id: ${cursor}\n` : ''}data: ${JSON.stringify({ schema: 'workspace.presentation.v1', workspaceId: workspace.id, sessionId: id, kind, ...body })}\n\n`));
}
function hydrate(id: string) {
  const content = `Readable ${id}`;
  const reference = { refId: `session-event:${id}-answer`, revision: '1', byteLength: String(encoder.encode(content).byteLength) };
  send(id, 'navigation', { snapshotId: id, offset: 0, hasMore: false, sessions: rows });
  send(id, 'snapshot', { page: { schema: 'transcript.page.v1', sessionId: id, projectionVersion: 'transcript.projection.v1',
    projectionGeneration: 'generation', sourceHighWater: '1', blocks: [{ blockId: `${id}-answer`, blockRevision: '1',
      orderKey: { sourceSequence: '1', ordinal: 0 }, body: { kind: 'assistantText', status: 'completed', content: { inlineContent: null, sourceRef: reference } } }],
    olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: 'workspace-transcript.v1', cursor: '1' }] } });
  send(id, 'content', { range: { schema: 'transcript.content.range.v1', sessionId: id, projectionVersion: 'transcript.projection.v1',
    projectionGeneration: 'generation', ...reference, startOffset: '0', endOffset: reference.byteLength, content, hasMore: false } });
  send(id, 'checkpoint', { projectionGeneration: 'generation', sourceHighWater: '1', agentRun: null }, `checkpoint:${id}`);
}
Object.assign(window, { sessionNavigationFixture: { requests, failures, mutations, pathname: () => router.state.location.pathname, ready: () => streams.has('session-4'),
  connected: (id: string) => streams.has(id), hydrate, navigate: (id: string) => router.navigate(`/w/workspace/sessions/${id}`),
  trash: () => router.navigate('/w/workspace/settings/archive?view=deleted'),
  library: () => router.navigate('/w/workspace/library'),
  task: () => router.navigate('/w/workspace/work/tasks/CW-12'),
  reports: () => router.navigate('/w/workspace/reports'),
  preferences: () => router.navigate('/w/workspace/settings/preferences'),
  back: () => router.navigate(-1), forward: () => router.navigate(1), search: () => router.state.location.search,
  repeatNavigation: (id: string) => send(id, 'navigation', { snapshotId: `${id}-repeat`, offset: 0, hasMore: false, sessions: rows }),
  live: (id: string) => send(id, 'live', { agentRunId: 'run', live: { messageId: 'live-answer', turnId: 'turn', afterSequence: 1,
    revision: '1', text: 'Incremental live answer', reasoning: null } }),
} });
createRoot(document.getElementById('fixture')!).render(<FrontendProvider iconStrokeWidth={2}><RouterProvider router={router} /></FrontendProvider>);
