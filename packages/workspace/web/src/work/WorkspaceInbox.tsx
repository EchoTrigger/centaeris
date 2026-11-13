import { useEffect, useRef, useState } from "react";
import { useLocation, useSearchParams } from "react-router";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";
import { Inbox, type InboxItem, type InboxView } from "./Inbox";

type Notification = {
  id: string; workspaceId: string; issueId: string; issueTitle: string;
  kind: "assigned" | "statusChanged";
  event: { actor: { id: string; name: string }; changes: Record<string, { before: unknown; after: unknown }> };
  readAt: string | null; archivedAt: string | null; createdAt: string;
};
type Page = { notifications: Notification[]; total: number; unreadCount: number };
export function WorkspaceInbox({ workspaceId, userId }: { workspaceId: string; userId: string }) {
  const { t } = useTranslation();
  const location = useLocation();
  const [params, setParams] = useSearchParams();
  const view = params.get("view") || "inbox";
  if (!["inbox", "unread", "archived"].includes(view)) throw new Error(`unsupported inbox view: ${view}`);
  const offset = Math.max(0, Number.parseInt(params.get("offset") || "0", 10) || 0);
  const selectedId = params.get("notification");
  const root = `/api/workspaces/${encodeURIComponent(workspaceId)}/notifications`;
  const [page, setPage] = useState<Page | null>(null);
  const [selected, setSelected] = useState<Notification | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [revision, setRevision] = useState(0);
  const [focusRevision, setFocusRevision] = useState(0);
  const owner = useRef(0);
  const mutation = useRef<AbortController | null>(null);
  useEffect(() => {
    const refresh = () => { setRevision(value => value + 1); setFocusRevision(value => value + 1); };
    window.addEventListener("focus", refresh);
    return () => { window.removeEventListener("focus", refresh); mutation.current?.abort(); };
  }, []);
  // biome-ignore lint/correctness/useExhaustiveDependencies: A different query clears the previous page; receipt refreshes retain it.
  useEffect(() => { setPage(null); setError(null); }, [root, view, offset]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Revision reloads authoritative records after an accepted receipt or focus.
  useEffect(() => {
    const controller = new AbortController();
    void apiJson<Page>(`${root}?view=${view}&limit=50&offset=${offset}`, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setPage(result);
    }).catch(failure => { if (!controller.signal.aborted) setError(String(failure.message)); });
    return () => controller.abort();
  }, [root, view, offset, revision]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Focus rereads the selected notification from the server.
  useEffect(() => {
    const controller = new AbortController();
    const visit = ++owner.current;
    setSelected(null); setError(null); setPending(false);
    mutation.current?.abort();
    if (selectedId) void (async () => {
      try {
        const result = await apiJson<{ notification: Notification }>(`${root}/${encodeURIComponent(selectedId)}`, { signal: controller.signal });
        if (controller.signal.aborted) return;
        setSelected(result.notification);
        if (!result.notification.readAt) {
          setPending(true);
          const receipt = await apiJson<{ notification: Notification }>(`${root}/${encodeURIComponent(selectedId)}`, { ...jsonOptions("PATCH", { read: true }), signal: controller.signal });
          if (controller.signal.aborted) return;
          setSelected(receipt.notification); setRevision(value => value + 1);
        }
      } catch (failure) { if (!controller.signal.aborted) setError(String((failure as Error).message)); }
      finally { if (owner.current === visit && !controller.signal.aborted) setPending(false); }
    })();
    return () => { controller.abort(); mutation.current?.abort(); };
  }, [root, selectedId, focusRevision]);
  async function change(id: string, values: Pick<Partial<InboxItem>, "readAt" | "archivedAt">) {
    const controller = new AbortController(); mutation.current = controller;
    const visit = owner.current;
    setPending(true); setError(null);
    try {
      const body = values.readAt !== undefined ? { read: values.readAt !== null } : { archived: values.archivedAt !== null };
      const result = await apiJson<{ notification: Notification }>(`${root}/${encodeURIComponent(id)}`, { ...jsonOptions("PATCH", body), signal: controller.signal });
      if (controller.signal.aborted || owner.current !== visit) return;
      setSelected(result.notification); setRevision(value => value + 1);
    } catch (failure) { if (!controller.signal.aborted && owner.current === visit) setError(String((failure as Error).message)); }
    finally { if (!controller.signal.aborted && owner.current === visit) setPending(false); }
  }
  function select(id: string | null) {
    setParams(previous => { const next = new URLSearchParams(previous); if (id) next.set("notification", id); else next.delete("notification"); return next; });
  }
  function item(notification: Notification): InboxItem {
    const summary = notification.kind === "assigned" ? t("inbox.assigned", { actor: notification.event.actor.name }) : t("inbox.statusChanged", { actor: notification.event.actor.name, before: t(`issues.status.${notification.event.changes.status.before}`), after: t(`issues.status.${notification.event.changes.status.after}`) });
    const returnTo = `${location.pathname}${location.search}`;
    return { ...notification, title: notification.issueTitle, actor: notification.event.actor.name, occurredAt: notification.createdAt, summary, body: "", target: { label: t("work.issues"), href: `/w/${encodeURIComponent(workspaceId)}/work/tasks/${encodeURIComponent(notification.issueId)}?returnTo=${encodeURIComponent(returnTo)}` } };
  }
  return <Inbox scrollKey={`centaeris:inbox-scroll:${JSON.stringify([userId, workspaceId, view, offset])}`} items={page?.notifications.map(item) || []} selectedItem={selected ? item(selected) : null} view={view as InboxView} selectedId={selectedId} loading={!page} pending={pending} error={error} onSelect={select} onChange={(id, values) => { void change(id, values); }} onViewChange={next => setParams(next === "inbox" ? {} : { view: next })} pagination={{ offset, total: page?.total || 0, onPage: nextOffset => setParams(previous => { const next = new URLSearchParams(previous); next.set("offset", String(nextOffset)); next.delete("notification"); return next; }) }} />;
}
