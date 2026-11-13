import { useEffect, useState } from "react";
import { Link } from "react-router";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";

export function IssueConversationLink({ workspaceId, sessionId }) {
  const { t } = useTranslation();
  const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}/sessions/${encodeURIComponent(sessionId)}/issue-origin`;
  const [data, setData] = useState(null);
  const [error, setError] = useState(false);
  const [revision, setRevision] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Explicit retry reloads the authorized origin.
  useEffect(() => {
    const controller = new AbortController();
    setError(false); setData(null);
    apiJson(endpoint, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setData({ endpoint, origin: result.origin });
    }).catch(() => { if (!controller.signal.aborted) setError(true); });
    return () => controller.abort();
  }, [endpoint, revision]);
  const origin = data?.endpoint === endpoint ? data.origin : null;
  if (error) return <button className="issueConversationBack" type="button" onClick={() => setRevision(value => value + 1)}>{t("issues.retryOrigin")}</button>;
  return origin ? <Link className="issueConversationBack" to={`/w/${encodeURIComponent(workspaceId)}/work/tasks/${encodeURIComponent(origin.issueId)}?execution=${encodeURIComponent(origin.executionId)}`}>
    {t("issues.backToIssue")}
  </Link> : null;
}
