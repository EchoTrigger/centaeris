import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";
import { WorkBreadcrumb } from "../work/WorkBreadcrumb";
import { CatalogDetailView } from "../components/CatalogDetailView";

export function SpaceSources({ workspace }) {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const sourceId = params.get("source");
  const [sources, setSources] = useState(null);
  const [objects, setObjects] = useState(null);
  const [error, setError] = useState("");
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const endpoint = `/api/workspaces/${encodeURIComponent(workspace.id)}/sources`;
  useEffect(() => {
    const controller = new AbortController();
    setSources(null); setError("");
    apiJson(endpoint, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setSources(result.sources);
    }).catch(() => { if (!controller.signal.aborted) setError(t("space.loadFailed")); });
    return () => controller.abort();
  }, [endpoint, t]);
  useEffect(() => {
    setObjects(null); setError("");
    if (!sourceId) return;
    const controller = new AbortController();
    apiJson(`${endpoint}/${encodeURIComponent(sourceId)}/objects`, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setObjects(result.objects);
    }).catch(() => { if (!controller.signal.aborted) setError(t("space.loadFailed")); });
    return () => controller.abort();
  }, [endpoint, sourceId, t]);
  const source = sources?.find(item => item.id === sourceId);
  return <section className="catalogPage spaceSources">
    <header className="workHeader catalogHeader"><WorkBreadcrumb current={t("libraryObjectRoute.library")} />
      <Link className="catalogTextLink" to={`${base}/extensions?view=connections`}>{t("catalog.manageConnections")}</Link>
    </header>
    <nav className="catalogTabs" aria-label={t("libraryRoute.libraryType")}><Link to={`${base}/library`}>{t("space.materials")}</Link><Link to={`${base}/library?view=knowledge`} aria-current="page">{t("space.sources")}</Link></nav>
    <div className={`catalogWorkspace${sourceId ? " hasDetail" : ""}`}>
      <div className="catalogDirectory" hidden={Boolean(sourceId)}>
        {sources === null ? <p className="catalogEmpty" role={error ? "alert" : "status"}>{error || t("agentChat.loading")}</p> : <>
          <div className="catalogColumnLabels"><span>{t("libraryRoute.name")}</span><span>{t("space.access")}</span></div>
          <ul className="catalogRows">{sources.map(item => <li key={item.id}><Link to={`${base}/library?view=knowledge&source=${encodeURIComponent(item.id)}`} aria-current={sourceId === item.id ? "true" : undefined}>
            <span><strong>{item.name}</strong><small>{t(item.sourceType === "fileTree" ? "space.fileTree" : "space.uploadedFiles")}</small></span><span className="catalogMeta">{t(`space.access.${item.accessLevel}`)}</span>
          </Link></li>)}</ul>
          {!sources.length ? <p className="catalogEmpty">{t("space.noSources")}</p> : null}
        </>}
      </div>
      {sourceId ? <CatalogDetailView backLabel={t("space.backToSources")} title={source?.name || t("space.sources")} backTo={`${base}/library?view=knowledge`}>
        {error ? <p role="alert">{error}</p> : sources === null || objects === null ? <p className="catalogEmpty" role="status">{t("agentChat.loading")}</p> : source ? <>
          <p className="catalogDescription">{t(`space.access.${source.accessLevel}`)}</p>
          <div className="catalogTable" role="table" aria-label={source.name}>
            <div className="catalogTableRow catalogTableHead" role="row"><span role="columnheader">{t("libraryRoute.name")}</span><span role="columnheader">{t("work.status")}</span></div>
            {objects.map(item => <div className="catalogTableRow" role="row" key={item.id}><span role="cell">{item.displayPath || item.displayName}</span><span role="cell">{t(`space.status.${item.status}`, { defaultValue: item.status })}</span></div>)}
          </div>{!objects.length ? <p className="catalogEmpty">{t("space.noObjects")}</p> : null}
        </> : <p role="status">{t("space.notAvailable")}</p>}
      </CatalogDetailView> : null}
    </div>
  </section>;
}
