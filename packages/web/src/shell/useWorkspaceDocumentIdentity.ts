import { useLayoutEffect } from "react";

export function useWorkspaceDocumentIdentity(workspace: Readonly<{ id: string; name: string }>) {
  useLayoutEffect(() => {
    const title = document.title;
    const existing = document.head.querySelector<HTMLLinkElement>('link[rel~="icon"]');
    const icon = existing ?? document.createElement("link");
    const href = icon.getAttribute("href");
    const type = icon.getAttribute("type");
    document.title = workspace.name;
    icon.setAttribute("href", "/centaeris-mark.png");
    icon.setAttribute("type", "image/png");
    if (!existing) { icon.rel = "icon"; document.head.append(icon); }
    return () => {
      document.title = title;
      if (!existing) icon.remove();
      else {
        if (href === null) icon.removeAttribute("href"); else icon.setAttribute("href", href);
        if (type === null) icon.removeAttribute("type"); else icon.setAttribute("type", type);
      }
    };
  }, [workspace.name]);
}
