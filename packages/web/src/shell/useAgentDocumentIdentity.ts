import { useLayoutEffect } from "react";
import { agentAvatarSrc, builtinAgentAvatarSrc, type AgentAvatarIdentity } from "./agentAvatar";

type Identity = AgentAvatarIdentity & Readonly<{ name: string }>;
type Entry = { title: string; src: string };
type HeadIdentity = {
  title: string; icon: HTMLLinkElement; href: string | null; type: string | null; created: boolean;
  entries: Map<symbol, Entry>;
};
const identities = new WeakMap<Document, HeadIdentity>();

function attribute(element: Element, name: string, value: string | null) {
  if (value === null) element.removeAttribute(name);
  else element.setAttribute(name, value);
}

function apply(document: Document, identity: HeadIdentity) {
  const current = Array.from(identity.entries.values()).at(-1);
  if (current) {
    document.title = current.title;
    identity.icon.setAttribute("href", current.src);
    identity.icon.setAttribute("type", "image/png");
  } else {
    document.title = identity.title;
    if (identity.created) identity.icon.remove();
    else { attribute(identity.icon, "href", identity.href); attribute(identity.icon, "type", identity.type); }
    identities.delete(document);
  }
}

function claim(document: Document, entry: Entry) {
  let identity = identities.get(document);
  if (!identity) {
    const existing = document.head.querySelector<HTMLLinkElement>('link[rel~="icon"]');
    const icon = existing ?? document.createElement("link");
    identity = { title: document.title, icon, href: icon.getAttribute("href"), type: icon.getAttribute("type"), created: !existing, entries: new Map() };
    if (!existing) { icon.rel = "icon"; document.head.append(icon); }
    identities.set(document, identity);
  }
  const token = Symbol("Agent tab");
  identity.entries.set(token, entry);
  const held = identity;
  apply(document, held);
  return {
    update(src: string) { if (held.entries.has(token)) { entry.src = src; apply(document, held); } },
    release() { held.entries.delete(token); apply(document, held); },
  };
}

export function useAgentDocumentIdentity(agent: Identity | null) {
  const name = agent?.name;
  const kind = agent?.avatarKind;
  const avatarUrl = agent?.avatarUrl;
  useLayoutEffect(() => {
    if (!name || !kind || typeof document === "undefined") return undefined;
    const identity = { name, avatarKind: kind, avatarUrl };
    const fallback = builtinAgentAvatarSrc(kind);
    const lease = claim(document, { title: name, src: fallback });
    const source = agentAvatarSrc(identity);
    let image: HTMLImageElement | undefined;
    if (source !== fallback) {
      image = new Image();
      image.onload = () => lease.update(source);
      image.onerror = () => lease.update(fallback);
      image.src = source;
    }
    return () => {
      if (image) { image.onload = null; image.onerror = null; }
      lease.release();
    };
  }, [name, kind, avatarUrl]);
}

export function AgentDocumentIdentity({ agent }: Readonly<{ agent: Identity | null }>) {
  useAgentDocumentIdentity(agent);
  return null;
}
