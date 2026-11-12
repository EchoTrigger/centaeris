import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import { useTranslation } from "../i18n";
import { Inbox, type InboxItem, type InboxView } from "./Inbox";

type Receipt = { readAt: string | null; archivedAt: string | null };
type DemoState = { startedAt: number; receipts: Record<string, Receipt> };
const examples = [
  { id: "review", actor: "Agent", title: "work.example.review.title", age: 12 * 60_000, target: "tasks/CW-12", label: "CW-12" },
  { id: "permissions", actor: "Agent", title: "work.example.permissions.title", age: 2 * 3_600_000, target: "tasks/CW-11", label: "CW-11" },
  { id: "assignment", actor: "Chen Yue", title: "work.example.releaseNotes.title", age: 6 * 3_600_000, target: "tasks/CW-9", label: "CW-9" },
  { id: "sources", actor: "Agent", title: "work.example.sources.title", age: 26 * 3_600_000, target: "tasks/CW-7", label: "CW-7" },
];
function readState(key: string): DemoState {
  try {
    const saved = JSON.parse(sessionStorage.getItem(key) || "null");
    if (saved && Number.isFinite(saved.startedAt) && saved.receipts && typeof saved.receipts === "object") {
      const receipts: Record<string, Receipt> = {};
      for (const item of examples) {
        const receipt = saved.receipts[item.id];
        if (receipt && [receipt.readAt, receipt.archivedAt].every(value => value === null || typeof value === "string")) receipts[item.id] = receipt;
      }
      return { startedAt: saved.startedAt, receipts };
    }
  } catch { /* Browser storage may be disabled; the demo still works in memory. */ }
  return { startedAt: Date.now(), receipts: {} };
}

// Only this adapter knows about sample content and browser persistence.
// A server-backed provider can supply the same items and acknowledgement callbacks.
export function DemoInbox({ userId, workspaceId }: { userId: string; workspaceId: string }) {
  const { t } = useTranslation();
  const [params, setParams] = useSearchParams();
  const key = `centaeris:inbox-demo:v1:${JSON.stringify([userId, workspaceId])}`;
  const [state, setState] = useState(() => readState(key));
  useEffect(() => {
    try { sessionStorage.setItem(key, JSON.stringify(state)); } catch { /* Keep this tab usable without storage. */ }
  }, [key, state]);
  const view = params.get("view") || "inbox";
  if (!["inbox", "unread", "archived"].includes(view)) throw new Error(`unsupported inbox view: ${view}`);
  const selectedId = params.get("notification");
  const items: InboxItem[] = examples.map(item => ({
    id: item.id, actor: item.actor, title: t(item.title), summary: t(`inbox.demo.${item.id}.summary`), body: t(`inbox.demo.${item.id}.body`),
    occurredAt: new Date(state.startedAt - item.age).toISOString(),
    ...(state.receipts[item.id] ?? { readAt: null, archivedAt: null }),
    target: { label: item.label, href: `/w/${encodeURIComponent(workspaceId)}/work/${item.target}` },
  }));
  function change(id: string, values: Partial<Receipt>) {
    setState(current => ({ ...current, receipts: { ...current.receipts, [id]: { ...(current.receipts[id] ?? { readAt: null, archivedAt: null }), ...values } } }));
  }
  function select(id: string | null) {
    setParams(previous => { const next = new URLSearchParams(previous); if (id) next.set("notification", id); else next.delete("notification"); return next; });
    if (id && !state.receipts[id]?.readAt) change(id, { readAt: new Date().toISOString() });
  }
  return <Inbox items={items} view={view as InboxView} selectedId={selectedId} onSelect={select} onChange={(id, values) => {
    change(id, values);
    if (values.archivedAt !== undefined) select(null);
  }} onViewChange={nextView => setParams(nextView === "inbox" ? {} : { view: nextView })} />;
}
