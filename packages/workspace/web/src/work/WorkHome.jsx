import { Link } from "react-router";
import { useEffect, useRef, useState } from "react";
import { Check, Circle, X } from "lucide-react";
import { Menu } from "@centaeris/ui/primitives/menu";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";
import { WorkBreadcrumb } from "./WorkBreadcrumb";

export function WorkHome({ workspace }) {
  const { t } = useTranslation();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const endpoint = `/api/workspaces/${encodeURIComponent(workspace.id)}/guide`;
  const [guide, setGuide] = useState(null);
  const [error, setError] = useState(null);
  const [pending, setPending] = useState(false);
  const request = useRef(null);
  const saving = useRef(false);
  const reload = useRef(null);
  useEffect(() => {
    async function load() {
      if (saving.current) return;
      request.current?.abort();
      const controller = new AbortController();
      request.current = controller;
      try {
        const result = await apiJson(endpoint, { signal: controller.signal });
        if (!controller.signal.aborted) { setGuide(result.guide); setError(null); }
      } catch (failure) { if (!controller.signal.aborted) setError(failure.message); }
    }
    reload.current = load;
    void load();
    window.addEventListener("focus", load);
    return () => { request.current?.abort(); window.removeEventListener("focus", load); reload.current = null; };
  }, [endpoint]);
  async function setDismissed(dismissed) {
    if (saving.current) return;
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    saving.current = true; setPending(true); setError(null);
    try {
      const result = await apiJson(endpoint, { ...jsonOptions("PATCH", { dismissed }), signal: controller.signal });
      if (!controller.signal.aborted) setGuide(result.guide);
    } catch (failure) { if (!controller.signal.aborted) setError(failure.message); }
    finally { if (!controller.signal.aborted) { saving.current = false; setPending(false); } }
  }
  const destinations = {
    models: `${base}/settings/models`, materials: `${base}/library`, plugins: `${base}/extensions`,
    connections: `${base}/extensions?view=connections`, conversation: `${base}/app`, issue: `${base}/work/tasks/new`,
  };
  const resources = [
    ["space", `${base}/library`, "libraryObjectRoute.library"],
    ["connections", `${base}/extensions?view=connections`, "catalog.manageConnections"],
    ["skills", `${base}/extensions?view=skills`, "extensions.skills"],
    ["projects", `${base}/work/projects`, "work.projects"],
  ];
  return <>
    <header className="workHeader"><WorkBreadcrumb current={t("work.home")} />
      <Menu.Root><Menu.Trigger className="homeHelp">{t("guide.help")}</Menu.Trigger>
        <Menu.Portal><Menu.Positioner className="workMenuPositioner" align="end" sideOffset={5}>
          <Menu.Popup className="workMenu"><Menu.Item className="workMenuItem" disabled={pending} onClick={() => void setDismissed(false)}>{t("guide.open")}</Menu.Item></Menu.Popup>
        </Menu.Positioner></Menu.Portal>
      </Menu.Root>
    </header>
    <div className="workHomeBody"><h2>{workspace.name}</h2>
      {error ? <div className="homeGuideError" role="alert"><span>{error}</span><button type="button" disabled={pending} onClick={() => void reload.current?.()}>{t("agentChat.retry")}</button></div> : null}
      {guide && !guide.dismissed ? <section className="homeGuide" aria-labelledby="home-guide-title" aria-busy={pending}>
        <header><h3 id="home-guide-title">{t("guide.title")}</h3><button type="button" disabled={pending} aria-label={t("guide.remove")} onClick={() => void setDismissed(true)}><X aria-hidden="true" /></button></header>
        <ul>{guide.steps.map(step => <li key={step.id}>
          <Link to={step.id === "issue" && step.completed ? `${base}/work/mine` : destinations[step.id]}>
            {step.completed ? <Check aria-label={t("guide.completed")} /> : <Circle aria-hidden="true" />}
            <span><strong>{t(`guide.${step.id}`)}</strong><small>{t(`guide.${step.id}Description`)}</small></span>
          </Link>
        </li>)}</ul>
      </section> : null}
      <section aria-labelledby="home-resources"><h3 id="home-resources">{t("home.resources")}</h3>
        <ul className="catalogRows">{resources.map(([id, href, label]) => <li key={id}><Link to={href}><span><strong>{t(label)}</strong></span><span aria-hidden="true">↗</span></Link></li>)}</ul>
      </section>
    </div>
  </>;
}
