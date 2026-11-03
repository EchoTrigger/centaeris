import { useLayoutEffect } from "react";
import { apiUrl } from "../api";
import { cacheWorkspaceIdentity } from "./workspaceIdentityCache";

export function workspaceInitialIcon(name: string) {
  const initial = (Array.from(name)[0] || "").replace(/[<>&"']/g, character => `&#${character.codePointAt(0)};`);
  return `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#eceef0"/><text x="16" y="22" text-anchor="middle" font-family="sans-serif" font-size="22" fill="#292b30">${initial}</text></svg>`)}`;
}

export function useWorkspaceDocumentIdentity(workspace: Readonly<{ id: string; name: string; avatarUrl: string | null }>) {
  useLayoutEffect(() => {
    const existing = Array.from(document.head.querySelectorAll<HTMLLinkElement>('link[rel~="icon"]'));
    const title = existing.some(icon => icon.hasAttribute("data-workspace-identity")) ? "Centaeris" : document.title;
    for (const previous of existing) {
      if (!previous.hasAttribute("data-workspace-identity")) continue;
      previous.setAttribute("href", "/centaeris-mark.png");
      previous.setAttribute("type", "image/png");
      previous.removeAttribute("data-workspace-identity");
    }
    for (const previous of existing) previous.remove();
    const icon = document.createElement("link");
    icon.rel = "icon";
    document.title = workspace.name;
    icon.setAttribute("href", workspace.avatarUrl ? apiUrl(workspace.avatarUrl) : workspaceInitialIcon(workspace.name));
    if (workspace.avatarUrl) icon.removeAttribute("type");
    else icon.setAttribute("type", "image/svg+xml");
    document.head.append(icon);
    cacheWorkspaceIdentity({ id: workspace.id, name: workspace.name, avatarUrl: workspace.avatarUrl });
    return () => {
      document.title = title;
      icon.remove();
      document.head.append(...existing);
    };
  }, [workspace.id, workspace.name, workspace.avatarUrl]);
}
