import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";
import ModelSettings from "./ModelSettings";
import PluginSettings from "./PluginSettings";

export default function OperatorWorkspaceResources({ isSuperuser }) {
  const { t } = useTranslation();
  const [workspaces, setWorkspaces] = useState(null);
  const [selectedId, setSelectedId] = useState("");
  const [panel, setPanel] = useState("models");
  const pending = useRef(null);
  const [failed, setFailed] = useState(false);

  const loadWorkspaces = useCallback(() => {
    if (!isSuperuser) return;
    pending.current?.abort();
    const controller = new AbortController();
    pending.current = controller;
    setWorkspaces(null);
    setFailed(false);
    apiJson("/api/admin/workspace-resources", { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setWorkspaces(result.workspaces); })
      .catch(() => { if (!controller.signal.aborted) setFailed(true); });
  }, [isSuperuser]);

  useEffect(() => {
    loadWorkspaces();
    return () => pending.current?.abort();
  }, [loadWorkspaces]);

  if (!isSuperuser) return null;
  const workspace = workspaces?.find(item => item.id === selectedId && item.status === "active");
  return <section className="capabilitySettings operatorWorkspaceResources">
    <header><h1>{t("workspaceResources.platformWorkspaces")}</h1><p>{t("workspaceResources.operatorHint")}</p></header>
    <div className="operatorResourceControls">
      {failed ? <div className="capabilitySettingsError" role="alert">{t("workspaceResources.loadFailed")} <button className="pluginEnableButton" type="button" onClick={loadWorkspaces}>{t("workspaceResources.retry")}</button></div> : null}
      <label className="workspaceModelsField"><span>{t("workspaceResources.selectWorkspace")}</span>
        <select value={selectedId} disabled={workspaces === null} onChange={event => setSelectedId(event.target.value)}>
          <option value="">{t("workspaceResources.selectWorkspace")}</option>
          {(workspaces || []).map(item => <option key={item.id} value={item.id} disabled={item.status !== "active"}>{item.name} ({item.status})</option>)}
        </select>
      </label>
      {workspace ? <div className="operatorResourceTabs" role="group" aria-label={t("workspaceResources.platformWorkspaces")}>
          <button type="button" aria-pressed={panel === "models"} onClick={() => setPanel("models")}>{t("workspaceResources.models")}</button>
          <button type="button" aria-pressed={panel === "plugins"} onClick={() => setPanel("plugins")}>{t("workspaceResources.plugins")}</button>
        </div> : null}
    </div>
    {workspace ? <div className={`operatorResourcePanel is${panel === "models" ? "Models" : "Plugins"}`} role="region" aria-label={t(`workspaceResources.${panel}`)}>
      {panel === "models" ? <ModelSettings key={`${workspace.id}:models`} workspace={workspace} onClose={() => setSelectedId("")} onModelsChanged={() => undefined} /> : <PluginSettings key={`${workspace.id}:plugins`} workspace={workspace} isSuperuser />}
    </div> : null}
  </section>;
}
