const prefix = "centaeris:workspace-chat:v1:";

export type ChatLayout = {
  dockedSessionIds: string[];
  activeSessionId: string | null;
  floating: boolean;
  lastCatalog: string;
};

const emptyLayout: ChatLayout = { dockedSessionIds: [], activeSessionId: null, floating: false, lastCatalog: "" };

function storageKey(userId: string, workspaceId: string, kind: string, scope = "") {
  return prefix + JSON.stringify([userId, workspaceId, kind, scope]);
}

function read(key: string): unknown {
  try { return JSON.parse(sessionStorage.getItem(key) || "null"); }
  catch { return null; }
}

function write(key: string, value: unknown) {
  try {
    if (value === "") sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, JSON.stringify(value));
  } catch { /* Browser state is optional when storage is disabled or full. */ }
}

export function readChatLayout(userId: string, workspaceId: string): ChatLayout {
  const value = read(storageKey(userId, workspaceId, "layout")) as ChatLayout | null;
  if (!value || Object.keys(value).sort().join("|") !== "activeSessionId|dockedSessionIds|floating|lastCatalog"
    || !Array.isArray(value.dockedSessionIds) || !value.dockedSessionIds.every(id => typeof id === "string" && id.length > 0)
    || !(value.activeSessionId === null || typeof value.activeSessionId === "string")
    || typeof value.floating !== "boolean" || typeof value.lastCatalog !== "string") return emptyLayout;
  return value;
}

export function writeChatLayout(userId: string, workspaceId: string, value: ChatLayout) {
  write(storageKey(userId, workspaceId, "layout"), value);
}

// Drafts are scoped to one browser tab, user, workspace and composer. No transcript is stored.
export function createChatDraft(userId: string, workspaceId: string, scope: string) {
  const key = storageKey(userId, workspaceId, "draft", scope);
  const saved = read(key);
  let value = typeof saved === "string" ? saved : "";
  const listeners = new Set<() => void>();
  return {
    getSnapshot: () => value,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    setDraft(next: string | ((previous: string) => string)) {
      value = typeof next === "function" ? next(value) : next;
      write(key, value);
      listeners.forEach(listener => listener());
    },
  };
}

export function clearBrowserChatState() {
  try {
    const keys = Object.keys(sessionStorage).filter(key => key.startsWith(prefix));
    for (const key of keys) sessionStorage.removeItem(key);
  } catch { /* Storage may be unavailable during logout. */ }
}
