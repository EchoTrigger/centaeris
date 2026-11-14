import { useEffect, useRef, useState } from "react";
import { Button } from "@centaeris/ui/button";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";
import { CatalogDetailView } from "../components/CatalogDetailView";
import { HighlightedTranscriptText, TranscriptSearchQuery } from "../chat/TranscriptSearchHighlight";

export function SpaceMaterialView({ workspaceId, sourceId, objectId, representationId, offset, highlight, kind = "sourceObject" }) {
  const { t } = useTranslation();
  const [data, setData] = useState(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  const continuation = useRef(null);
  const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}/search/materials/${kind}/${encodeURIComponent(objectId)}`;
  const backTo = `/w/${encodeURIComponent(workspaceId)}/library${kind === "sourceObject" ? `?${new URLSearchParams({ view: "knowledge", source: sourceId })}` : ""}`;
  // biome-ignore lint/correctness/useExhaustiveDependencies: Reload explicitly rereads current material permission and content.
  useEffect(() => {
    const controller = new AbortController();
    setData(null); setError(""); setLoading(true);
    const query = new URLSearchParams({ offset: String(offset) });
    if (representationId) query.set("representationId", representationId);
    apiJson(`${endpoint}?${query}`, { signal: controller.signal }).then(result => { if (!controller.signal.aborted) setData(result); })
      .catch(failure => { if (!controller.signal.aborted) setError(failure.message); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => { controller.abort(); continuation.current?.abort(); };
  }, [endpoint, representationId, offset, revision]);
  async function more() {
    const controller = new AbortController(); continuation.current = controller;
    setLoading(true); setError("");
    try {
      const result = await apiJson(`${endpoint}?${new URLSearchParams({ representationId: data.representationId, offset: String(data.nextOffset) })}`, { signal: controller.signal });
      if (!controller.signal.aborted) setData(previous => ({ ...result, content: previous.content + result.content }));
    } catch (failure) { if (!controller.signal.aborted) { setError(failure.message); if ([403, 404].includes(failure.status)) setData(null); } }
    finally { if (!controller.signal.aborted) setLoading(false); }
  }
  return <CatalogDetailView backTo={backTo} backLabel={t(kind === "sourceObject" ? "space.backToSources" : "libraryObjectRoute.library")} title={data?.material.displayName || ""}>
    {error ? <div role="alert" className="issueExecutionError"><span>{error}</span><Button variant="ghost" onClick={() => setRevision(value => value + 1)}>{t("issues.reload")}</Button></div> : null}
    {data ? <TranscriptSearchQuery value={highlight || ""}><pre className="spaceMaterialText"><HighlightedTranscriptText text={data.content} /></pre></TranscriptSearchQuery> : null}
    {data?.nextOffset !== null && data?.nextOffset !== undefined ? <Button variant="ghost" disabled={loading} onClick={() => void more()}>{t("searchOverlay.loadMoreResources")}</Button> : null}
  </CatalogDetailView>;
}
