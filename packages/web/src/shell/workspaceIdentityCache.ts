export const WORKSPACE_IDENTITY_CACHE_KEY = "centaeris:workspace-identity:v1";

export function cacheWorkspaceIdentity(workspace: Readonly<{ id: string; name: string; avatarUrl: string | null }>) {
  try {
    sessionStorage.setItem(WORKSPACE_IDENTITY_CACHE_KEY, JSON.stringify({
      schema: "workspace.identity.v1", workspaceId: workspace.id,
      name: workspace.name, avatarUrl: workspace.avatarUrl,
    }));
  } catch { /* Presentation preferences never grant access. */ }
}

export function clearWorkspaceIdentityCache() {
  try { sessionStorage.removeItem(WORKSPACE_IDENTITY_CACHE_KEY); } catch { /* Storage may be unavailable. */ }
  const bootstrapIcon = document.head.querySelector<HTMLLinkElement>("link[data-workspace-identity]");
  if (bootstrapIcon) {
    document.title = "Centaeris";
    bootstrapIcon.href = "/centaeris-mark.png";
    bootstrapIcon.type = "image/png";
    bootstrapIcon.removeAttribute("data-workspace-identity");
  }
}
