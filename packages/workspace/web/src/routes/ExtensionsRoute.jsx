import { useEffect, useState } from "react";
import { Link, useRouteLoaderData, useSearchParams } from "react-router";
import { ChevronRight, LoaderCircle } from "lucide-react";
import { apiJson } from "../api";
import { MarkdownContent } from "../chat/MarkdownContent";
import { useTranslation } from "../i18n";
import PluginSettings from "./PluginSettings";
import { CatalogDetailView } from "../components/CatalogDetailView";
import "../catalog.css";

function SkillDetail({ path, skillId, backTo }) {
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
  return <CatalogDetailView backLabel={t("extensions.backToSkills")} title={detail?.skill.name || t("extensions.skills")} backTo={backTo}>
    {detail?.skill.description ? <p className="catalogDescription">{detail.skill.description}</p> : null}
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
  </CatalogDetailView>;
}

function ExtensionCatalog({ workspace, view, isSuperuser, detailId, onSelectPlugin }) {
  const { t } = useTranslation();
  const [items, setItems] = useState(null);
  const [error, setError] = useState("");
  const skills = view === "skills";
  const id = encodeURIComponent(workspace.id);
  const base = `/w/${id}/extensions`;
  const skillList = `${base}?view=skills`;
  const path = `/api/workspaces/${id}/skills`;
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

  const navigation = <nav className="catalogTabs" aria-label={t("extensions.title")}>
    <Link to={base} aria-current={view === "plugins" ? "page" : undefined}>{t("catalog.installed")}</Link>
    <Link to={skillList} aria-current={skills ? "page" : undefined}>{t("extensions.skills")}</Link>
    <Link to={`${base}?view=connections`} aria-current={view === "connections" ? "page" : undefined}>{t("catalog.connections")}</Link>
  </nav>;

  return <section className="extensionPage catalogPage" aria-label={t("extensions.title")}>
    {!skills ? <PluginSettings workspace={workspace} isSuperuser={isSuperuser}
      catalog={{ selectedName: detailId, onSelect: onSelectPlugin, navigation, base, view }} />
      : <>
        <header className="workHeader catalogHeader"><h1>{t("extensions.title")}</h1></header>
        {navigation}
        <div className={`catalogWorkspace${detailId ? " hasDetail" : ""}`}>
        <div className="catalogDirectory" hidden={Boolean(detailId)}>
          {error ? <p className="catalogEmpty" role="alert">{error}</p> : items === null
            ? <p className="catalogEmpty" role="status">{t("agentChat.loading")}</p>
            : items.length === 0 ? <p className="catalogEmpty">{t("libraryRoute.noSkillsAvailableInThisWorkspace")}</p>
            : <ul className="catalogRows">{items.map(item => <li key={item.skillId}>
              <Link to={`${skillList}&${new URLSearchParams({ skill: item.skillId })}`} aria-current={detailId === item.skillId ? "true" : undefined}>
                <span><strong>{item.name}</strong><small>{item.description}</small></span>
                <span className="catalogMeta">{t(item.skillId.startsWith("system:") ? "libraryRoute.system" : "libraryRoute.plugins")}<ChevronRight aria-hidden="true" /></span>
              </Link>
            </li>)}</ul>}
        </div>
        {detailId ? <SkillDetail key={detailId} path={path} skillId={detailId} backTo={skillList} /> : null}
        </div>
      </>}
  </section>;
}

export default function ExtensionsRoute() {
  const { workspace } = useRouteLoaderData("workspace");
  const { user } = useRouteLoaderData("authenticated");
  const [params, setParams] = useSearchParams();
  const view = params.get("view") || "plugins";
  if (!["plugins", "skills", "connections"].includes(view)) throw new Error(`unsupported extensions view: ${view}`);
  return <ExtensionCatalog key={`${workspace.id}:${view}`} workspace={workspace} view={view} isSuperuser={user.isSuperuser}
    detailId={params.get(view === "skills" ? "skill" : "plugin")} onSelectPlugin={plugin => setParams(view === "plugins" ? { plugin } : { view, plugin })} />;
}
