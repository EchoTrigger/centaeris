export function sortSessionNavigation(sessions) {
  return [...sessions].sort((left, right) => (
    Number(Boolean(right.isPinned)) - Number(Boolean(left.isPinned))
    || String(right.updatedAt || "").localeCompare(String(left.updatedAt || ""))
  ));
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
