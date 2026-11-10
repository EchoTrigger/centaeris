import { useEffect, useRef, useState } from "react";
import { Link, useRouteLoaderData, useSearchParams } from "react-router";
import { ArrowLeft, ChevronRight, Layers, LoaderCircle, Search } from "lucide-react";
import { apiJson } from "../api";
import { MarkdownContent } from "../chat/MarkdownContent";
import { useTranslation } from "../i18n";
import PluginSettings from "./PluginSettings";

function SkillDetail({ path, skillId, back }) {
  const { t } = useTranslation();
  const [detail, setDetail] = useState(null);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    apiJson(`${path}/${encodeURIComponent(skillId)}`, { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setDetail(result); })
      .catch(() => {
        if (!controller.signal.aborted) setError(t("libraryRoute.unableToOpenSkill"));
      });
    return () => controller.abort();
  }, [path, skillId, t]);
  return <>
    {back}
    <header className="extensionHeading"><div><h1 tabIndex={-1}>{detail?.skill.name || t("extensions.skills")}</h1>
      {detail?.skill.description ? <p>{detail.skill.description}</p> : null}</div></header>
    {error ? <div className="settingsPanel libraryCatalogEmpty" role="alert">{error}</div> : detail ? <>
      <section className="settingsPanel extensionSkillFacts">
        <dl>
          <div><dt>{t("libraryRoute.source")}</dt><dd>{detail.skill.skillId.startsWith("system:") ? t("libraryRoute.systemMount") : t("libraryRoute.pluginMount")}</dd></div>
          <div><dt>{t("libraryRoute.invocation")}</dt><dd>{detail.skill.allowImplicitInvocation ? t("libraryRoute.automaticOrExplicit") : t("libraryRoute.explicitOnly")}</dd></div>
          <div><dt>{t("libraryRoute.allowedTools")}</dt><dd>{detail.skill.allowedTools.length ? detail.skill.allowedTools.join(" · ") : t("libraryRoute.unrestricted")}</dd></div>
        </dl>
      </section>
      <article className="settingsPanel extensionSkillDocument"><MarkdownContent text={detail.content.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "")} /></article>
    </> : <div className="settingsPanel libraryCatalogEmpty" role="status"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryRoute.openingSkill")}</div>}
  </>;
}

function ExtensionCatalog({ workspace, view, isSuperuser, detailId, onSelectPlugin }) {
  const { t } = useTranslation();
  const [items, setItems] = useState(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const skills = view === "skills";
  const id = encodeURIComponent(workspace.id);
  const base = `/w/${id}/extensions`;
  const skillList = `${base}?view=skills`;
  const path = `/api/workspaces/${id}/skills`;
  const pageRef = useRef(null);
  useEffect(() => {
    const page = pageRef.current;
    page?.closest(".workspaceCatalog")?.scrollTo({ top: 0 });
    if (detailId) page?.querySelector("h1")?.focus({ preventScroll: true });
  }, [detailId]);
  useEffect(() => {
    if (!skills) return;
    const controller = new AbortController();
    apiJson(path, { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setItems(result.skills); })
      .catch(() => {
        if (!controller.signal.aborted) setError(t("libraryRoute.unableToLoadSkills"));
      });
    return () => controller.abort();
  }, [path, skills, t]);

  const filter = query.trim().toLocaleLowerCase();
  const rows = (items || []).filter(item => `${item.name} ${item.description || ""}`.toLocaleLowerCase().includes(filter));
  const search = <label className="librarySearch extensionSearch"><Search aria-hidden="true" /><input aria-label={t("extensions.search")} value={query} onChange={event => setQuery(event.target.value)} placeholder={t("extensions.search")} /></label>;
  const back = <Link className="managementBack extensionBack" to={skills ? skillList : base}><ArrowLeft aria-hidden="true" />{t(skills ? "extensions.backToSkills" : "extensions.backToPlugins")}</Link>;

  return <section className="extensionPage" ref={pageRef} aria-label={t("extensions.title")}>
    <nav className="extensionTabs" aria-label={t("extensions.title")}>
      <Link to={base} aria-current={!skills ? "page" : undefined}>{t("libraryRoute.plugins")}</Link>
      <Link to={skillList} aria-current={skills ? "page" : undefined}>{t("extensions.skills")}</Link>
    </nav>
    {!skills ? <PluginSettings workspace={workspace} isSuperuser={isSuperuser} filter={filter}
      catalog={{ selectedName: detailId, onSelect: onSelectPlugin, search, back }} />
      : detailId ? <SkillDetail key={detailId} path={path} skillId={detailId} back={back} />
      : <>
        <header className="extensionHeading"><h1>{t("extensions.skills")}</h1></header>
        <div className="extensionToolbar">{search}</div>
        {error ? <div className="settingsPanel libraryCatalogEmpty" role="alert">{error}</div> : items === null
          ? <div className="settingsPanel libraryCatalogEmpty" role="status"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("agentChat.loading")}</div>
          : rows.length === 0 ? <div className="settingsPanel libraryCatalogEmpty"><p>{t(filter ? "extensions.noMatches" : "libraryRoute.noSkillsAvailableInThisWorkspace")}</p></div>
          : <ul className="settingsPanel settingsListPanel extensionList">{rows.map(item => <li key={item.skillId}>
            <Link className="managementRow managementRowButton" to={`${skillList}&${new URLSearchParams({ skill: item.skillId })}`}>
              <span className="managementRowIcon"><Layers aria-hidden="true" /></span>
              <span className="managementRowCopy"><strong>{item.name}</strong><small>{item.description}</small></span>
              <span className="managementRowEnd">{t(item.skillId.startsWith("system:") ? "libraryRoute.system" : "libraryRoute.plugins")}<ChevronRight aria-hidden="true" /></span>
            </Link>
          </li>)}</ul>}
      </>}
  </section>;
}

export default function ExtensionsRoute() {
  const { workspace } = useRouteLoaderData("workspace");
  const { user } = useRouteLoaderData("authenticated");
  const [params, setParams] = useSearchParams();
  const view = params.get("view") || "plugins";
  if (view !== "plugins" && view !== "skills") throw new Error(`unsupported extensions view: ${view}`);
  return <ExtensionCatalog key={`${workspace.id}:${view}`} workspace={workspace} view={view} isSuperuser={user.isSuperuser}
    detailId={params.get(view === "skills" ? "skill" : "plugin")} onSelectPlugin={plugin => setParams({ plugin })} />;
}
