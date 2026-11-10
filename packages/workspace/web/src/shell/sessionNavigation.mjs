export function sortSessionNavigation(sessions) {
  const sorted = [...sessions].sort((left, right) => (
    Number(Boolean(right.isPinned)) - Number(Boolean(left.isPinned))
    || String(right.updatedAt || "").localeCompare(String(left.updatedAt || ""))
  ));
  return sorted.every((record, index) => record === sessions[index]) ? sessions : sorted;
}

export function groupSessionNavigation(sessions, projects) {
    const grouped = {
      pinned: [],
      recent: [],
      projectSessions: Object.fromEntries(projects.map((project) => [project.id, []])),
      projectionError: false,
    };
    sessions.forEach((session) => {
      if (session.origin !== "user" && session.origin !== "automation") {
        grouped.projectionError = true;
        return;
      }
      if (session.projectId) {
        const projectSessions = grouped.projectSessions[session.projectId];
        if (!projectSessions) {
          grouped.projectionError = true;
          return;
        }
        projectSessions.push(session);
      } else if (session.isPinned) grouped.pinned.push(session);
      else grouped.recent.push(session);
    });
    return grouped;
}

export function sessionChatPath(workspaceId, sessionId) {
  if (![workspaceId, sessionId].every(value => typeof value === "string" && value && value.trim() === value)) throw new Error("session_identity_invalid");
  return `/w/${encodeURIComponent(workspaceId)}/sessions/${encodeURIComponent(sessionId)}`;
}

export function isWorkspaceCatalogPath(base, pathname) {
  return pathname === `${base}/library` || pathname.startsWith(`${base}/library/`) || pathname === `${base}/extensions`;
}
