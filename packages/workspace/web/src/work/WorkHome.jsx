import { Link } from "react-router";
import { useTranslation } from "../i18n";
import { WorkBreadcrumb } from "./WorkBreadcrumb";

export function WorkHome({ workspace }) {
  const { t } = useTranslation();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const resources = [
    ["space", `${base}/library`, "libraryObjectRoute.library"],
    ["connections", `${base}/extensions?view=connections`, "catalog.manageConnections"],
    ["skills", `${base}/extensions?view=skills`, "extensions.skills"],
    ["projects", `${base}/work/projects`, "work.projects"],
  ];
  return <>
    <header className="workHeader"><WorkBreadcrumb current={t("work.home")} /></header>
    <div className="workHomeBody"><h2>{workspace.name}</h2>
      <section aria-labelledby="home-resources"><h3 id="home-resources">{t("home.resources")}</h3>
        <ul className="catalogRows">{resources.map(([id, href, label]) => <li key={id}><Link to={href}><span><strong>{t(label)}</strong><small>{t(`home.${id}`)}</small></span><span aria-hidden="true">↗</span></Link></li>)}</ul>
      </section>
    </div>
  </>;
}
