import { useEffect, useState } from "react";
import { ApiError, apiJson } from "../api";
import { FileLinkCard } from "../chat/FileLinkCard";
import { MarkdownContent } from "../chat/MarkdownContent";
import { useTranslation } from "../i18n";

type Deliverable = {
  id: string;
  revision: number;
  summary: string;
  recipient: { name: string };
  artifacts: { id: string; displayName: string; downloadUrl: string }[];
};

export function InboxDeliverable({ workspaceId, issueId, deliverableId, refreshRevision }: {
  workspaceId: string; issueId: string; deliverableId: string; refreshRevision: number;
}) {
  const { t } = useTranslation();
  const [deliverable, setDeliverable] = useState<Deliverable | null>(null);
  const [error, setError] = useState<string | null>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Window focus rechecks access while keeping the current result mounted.
  useEffect(() => {
    const controller = new AbortController();
    setError(null);
    const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}/issues/${encodeURIComponent(issueId)}/deliverables`;
    void apiJson<{ deliverables: Deliverable[] }>(endpoint, { signal: controller.signal }).then(result => {
      if (controller.signal.aborted) return;
      const item = result.deliverables.find(value => value.id === deliverableId);
      if (item) setDeliverable(item);
      else { setDeliverable(null); setError(t("deliverables.unavailable")); }
    }).catch(failure => {
      if (!controller.signal.aborted) {
        setDeliverable(null);
        setError(failure instanceof ApiError && [403, 404].includes(failure.status)
          ? t("deliverables.unavailable") : String(failure.message));
      }
    });
    return () => controller.abort();
  }, [workspaceId, issueId, deliverableId, refreshRevision, t]);
  if (error) return <p role="alert">{error}</p>;
  if (!deliverable) return <div className="inboxDeliverable" aria-busy="true"><p role="status">{t("inbox.loadingContent")}</p></div>;
  return <div className="inboxDeliverable">
    <div className="inboxDeliverableMeta"><span>{t("deliverables.revision", { revision: deliverable.revision })}</span><span>{t("deliverables.recipient")}：{deliverable.recipient.name}</span></div>
    {deliverable.summary ? <MarkdownContent text={deliverable.summary} /> : null}
    {deliverable.artifacts.length ? <ul className="inboxDeliverableFiles" aria-label={t("deliverables.files")}>{deliverable.artifacts.map(artifact => <li key={artifact.id}>
      <FileLinkCard filename={artifact.displayName} href={artifact.downloadUrl} download />
    </li>)}</ul> : null}
  </div>;
}
