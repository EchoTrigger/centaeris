import type { UiSession } from "../types/ui";
import type { WorkspaceInfo } from "../lib/workspaceBridge";

export const sortSessionCatalog = (sessions: UiSession[]): UiSession[] => [...sessions].sort((a, b) => {
  if (Boolean(a.isPinned) !== Boolean(b.isPinned)) return a.isPinned ? -1 : 1;
  const manual = a.isPinned ? (a.sortOrder ?? Number.MAX_SAFE_INTEGER) - (b.sortOrder ?? Number.MAX_SAFE_INTEGER) : 0;
  return manual || (b.updatedAt ?? 0) - (a.updatedAt ?? 0) || a.id.localeCompare(b.id);
});

const rootKey = (root: string) => {
  const path = root.replace(/^\\\\\?\\/, "").replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[a-z]:/i.test(path) || path.startsWith("//") ? path.toLowerCase() : path;
};

export function recentWorkspaceGroups(sessions: UiSession[], workspaces: WorkspaceInfo[]) {
  const groups = new Map<string, { root: string; name: string; sessions: UiSession[] }>();
  for (const workspace of workspaces) groups.set(rootKey(workspace.root), { root: workspace.root, name: workspace.name, sessions: [] });
  for (const session of sortSessionCatalog(sessions.filter(item => !item.isPinned))) {
    const root = session.cwd ?? "";
    const key = rootKey(root);
    groups.get(key)?.sessions.push(session);
  }
  return [...groups.values()].sort((a, b) => (b.sessions[0]?.updatedAt ?? 0) - (a.sessions[0]?.updatedAt ?? 0));
}
