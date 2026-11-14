import { createRoot } from "react-dom/client";
import { createMemoryRouter, RouterProvider } from "react-router";
import { FrontendProvider } from "@centaeris/ui/provider";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import WorkspaceChatLayout from "../../src/routes/WorkspaceHomeRoute";
import LibraryObjectRoute from "../../src/routes/LibraryObjectRoute";
import LibraryPage from "../../src/routes/LibraryRoute";
import ExtensionsRoute from "../../src/routes/ExtensionsRoute";
import WorkPrototypeRoute from "../../src/work/WorkPrototypeRoute";
import { ScrollActivity } from "../../src/components/ScrollActivity";
import "../../src/globals.css";

configureApi({ apiBaseUrl: location.origin });
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") || "light";
await i18n.changeLanguage("en");
const workspace = { id: "workspace", name: "Research workspace", role: "owner", avatarUrl: null };
const user = { id: "user", email: "fixture@example.invalid" };
const plugin = { name: "research", displayName: "Research tools", shortDescription: "Read source documents and assemble evidence.", version: "1.0.0", packageDigest: "digest", enabled: true, errors: [], capabilities: ["skills", "mcp"], mcpCredentialRefs: ["sources"], hooks: [], scope: "workspace", writable: true,
  mcpServers: [{ id: "Document service", transport: { type: "streamableHttp", endpoint: "https://example.invalid/mcp" }, auth: { type: "bearer", credentialRef: "sources", credentialConfigured: false }, tools: [] }, { id: "Search service", transport: { type: "streamableHttp", endpoint: "https://example.invalid/search" }, auth: { type: "bearer", credentialRef: "sources", credentialConfigured: false }, tools: [] }] };
const issue = { id: "issue-evidence", workspaceId: workspace.id, title: "Review evidence for the connector integration", description: "Check the source permissions and record the result.", status: "inProgress", assigneeId: user.id, creatorId: user.id, version: 1, archivedAt: null, createdAt: "2026-11-12T01:00:00Z", updatedAt: "2026-11-12T01:00:00Z" };
const skill = { skillId: "plugin:research:evidence", name: "Evidence review", description: "Check claims against the available sources.", allowImplicitInvocation: true, allowedTools: [] };
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
const notification = { id: "notification-evidence", workspaceId: workspace.id, issueId: issue.id, issueTitle: issue.title, kind: "assigned", event: { id: "assignment-evidence", issueId: issue.id, actor: { id: "other", name: "Alex" }, type: "updated", changes: { assigneeId: { before: null, after: user.id } }, createdAt: issue.createdAt }, readAt: null, archivedAt: null, createdAt: issue.createdAt, ...JSON.parse(localStorage.getItem("fixtureNotificationReceipt") || "{}") };
window.fetch = async (input, options) => {
  const url = new URL(String(input), location.href);
  if (url.pathname === "/api/csrf") return Response.json({ csrfToken: "fixture" });
  if (url.pathname.endsWith("/notifications/notification-evidence")) {
    if (options?.method === "PATCH") {
      const body = JSON.parse(String(options.body));
      if (body.read !== undefined) notification.readAt = body.read ? new Date().toISOString() : null;
      if (body.archived !== undefined) notification.archivedAt = body.archived ? new Date().toISOString() : null;
      localStorage.setItem("fixtureNotificationReceipt", JSON.stringify({ readAt: notification.readAt, archivedAt: notification.archivedAt }));
    }
    return Response.json({ notification });
  }
  if (url.pathname.endsWith("/notifications")) {
    const view = url.searchParams.get("view");
    const visible = view === "archived" ? notification.archivedAt : !notification.archivedAt && (view !== "unread" || !notification.readAt);
    return Response.json({ notifications: visible ? [notification] : [], total: visible ? 1 : 0, unreadCount: notification.readAt || notification.archivedAt ? 0 : 1 });
  }
  if (options?.method && options.method !== "GET") throw new Error(`Unexpected mutation: ${url.pathname}`);
  if (url.pathname.endsWith("/issues/assignees")) return Response.json({ assignees: [{ id: user.id, name: "Alex" }] });
  if (url.pathname.endsWith("/executions")) return Response.json({ executions: [], total: 0 });
  if (url.pathname.endsWith("/issues/issue-evidence/events")) return Response.json({ events: [{ id: "event-evidence", issueId: issue.id, actor: { id: user.id, name: "Alex" }, type: "created", changes: { title: { before: null, after: issue.title } }, createdAt: issue.createdAt }], total: 1 });
  if (url.pathname.endsWith("/issues/issue-evidence")) return Response.json({ issue });
  if (url.pathname.endsWith("/issues")) return Response.json({ issues: [issue], total: 1 });
  if (url.pathname === "/api/models") return Response.json({ models: [] });
  if (url.pathname.endsWith("/session-projects")) return Response.json({ projects: [] });
  if (url.pathname.endsWith("/sessions")) return Response.json({ sessions: [] });
  if (url.pathname.endsWith("/plugins")) return Response.json({ plugins: [plugin] });
  if (url.pathname.endsWith("/plugins/research")) return Response.json({ plugin });
  if (url.pathname.endsWith("/mcp-bearer-credentials")) return Response.json({ credentials: [] });
  if (url.pathname.endsWith("/skills")) return Response.json({ skills: [skill] });
  if (url.pathname.includes("/skills/")) return Response.json({ skill, content: "# Evidence review\n\nCompare each claim with its source before drawing a conclusion." });
  if (url.pathname.endsWith("/sources")) return Response.json({ sources: [{ id: "source", name: "Research documents", sourceType: "fileTree", status: "ready", accessLevel: "read" }] });
  if (url.pathname.endsWith("/sources/source/objects")) return Response.json({ objects: [{ id: "object", displayName: "Research brief.pdf", displayPath: "Research brief.pdf", status: "ready" }] });
  if (url.pathname === "/api/library/notes" || url.pathname === "/api/library/notes/note") return Response.json({ object: { id: "notes", displayName: "Review notes", objectKind: "note", contentType: "text/markdown", contentGeneration: 1, status: "ready" }, markdown: "Review the evidence before publishing." });
  if (url.pathname === "/api/library") return Response.json({ objects: [
    { id: "brief", displayName: "Research brief.pdf", objectKind: "file", contentType: "application/pdf", sizeBytes: 39200, status: "ready", updatedAt: "2026-11-12T10:00:00Z" },
    { id: "notes", displayName: "Review notes", objectKind: "note", contentType: "text/markdown", sizeBytes: 1500, status: "ready", updatedAt: "2026-11-12T09:00:00Z" },
  ] });
  throw new Error(`Unexpected catalog request: ${url.pathname}`);
};
const router = createMemoryRouter([{ id: "authenticated", loader: () => ({ user }), children: [{ id: "workspace", path: "/w/:workspaceId", loader: () => ({ workspace, workspaces: [workspace], selectedSession: null }),
  Component: () => <WorkspaceChatLayout />, children: [
    { path: "work/*", Component: WorkPrototypeRoute }, { path: "library", element: <LibraryPage embedded />, children: [{ path: ":libraryObjectId", element: <LibraryObjectRoute embedded inline /> }] }, { path: "extensions", Component: ExtensionsRoute },
  ],
}] }], { initialEntries: [new URLSearchParams(location.search).get("route") || "/w/workspace/work/inbox"] });
router.subscribe(state => { const url = new URL(location.href); url.searchParams.set("route", state.location.pathname + state.location.search); history.replaceState(null, "", url); });
Object.assign(window, { catalogFixture: { navigate: (path: string) => router.navigate(`/w/workspace/${path}`), back: () => router.navigate(-1), failures, language: (value: string) => i18n.changeLanguage(value) } });
createRoot(document.getElementById("fixture")!).render(<FrontendProvider iconStrokeWidth={1.8}><ScrollActivity /><RouterProvider router={router} /></FrontendProvider>);
