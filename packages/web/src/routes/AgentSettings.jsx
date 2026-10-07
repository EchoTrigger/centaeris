import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Link, useLocation, useNavigate, useRevalidator, useRouteLoaderData } from "react-router";
import { Plus, Pencil, Trash2 } from "lucide-react";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";
import { AgentMark } from "../shell/AgentMark";
import { AgentEditorModal } from "../shell/AgentEditorModal";
import { agentSettingsPath, agentSettingsSelection } from "../agent-chat/agentSettingsNavigation";
import AgentModelSettings from "./AgentModelSettings";
import "./agent-settings.css";

export default function AgentSettings({ onEditorOpenChange }) {
  const { t } = useTranslation();
  const { workspace, agents } = useRouteLoaderData("workspace");
  const location = useLocation(); const navigate = useNavigate(); const revalidator = useRevalidator();
  const selection = agentSettingsSelection(location.search, agents.map(agent => agent.id));
  const selected = selection.kind === "agent" ? agents.find(agent => agent.id === selection.agentId) : null;
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const mutationControllerRef = useRef(null);
  useEffect(() => () => mutationControllerRef.current?.abort(), []);
  const creating = selection.kind === "create";
  const editorOpen = creating || Boolean(selected && editing);
  useEffect(() => { onEditorOpenChange(editorOpen); return () => onEditorOpenChange(false); }, [editorOpen, onEditorOpenChange]);
  const base = agentSettingsPath(workspace.id);
  const navigateSettings = path => navigate(path, { state: location.state });
  const closeEditor = () => { if (creating) navigateSettings(base); else setEditing(false); };
  async function saveProfile(value) {
    if (busy) return;
    setBusy(true); setError("");
    const controller = new AbortController(); mutationControllerRef.current = controller;
    try {
      const result = await apiJson(creating ? `/api/workspaces/${encodeURIComponent(workspace.id)}/agents` : `/api/agents/${encodeURIComponent(selected.id)}`, { ...jsonOptions(creating ? "POST" : "PATCH", value), signal: controller.signal });
      if (controller.signal.aborted) return;
      if (!result?.agent?.id || result.agent.workspaceId !== workspace.id || (!creating && result.agent.id !== selected.id)) throw new Error("agent_settings_response_invalid");
      await revalidator.revalidate();
      if (controller.signal.aborted) return;
      setEditing(false);
      navigateSettings(agentSettingsPath(workspace.id, result.agent.id));
    } catch { if (!controller.signal.aborted) setError(t("agentSettings.profileError")); }
    finally { if (!controller.signal.aborted) setBusy(false); }
  }
  async function remove() {
    if (busy || !selected) return;
    setBusy(true); setError("");
    const controller = new AbortController(); mutationControllerRef.current = controller;
    try {
      await apiJson(`/api/agents/${encodeURIComponent(selected.id)}`, { method: "DELETE", signal: controller.signal });
      if (controller.signal.aborted) return;
      // Clear explicit selection before revalidation removes its directory row.
      await navigateSettings(base); await revalidator.revalidate();
    } catch (failure) { if (!controller.signal.aborted) setError(failure.message === "agent_has_active_agent_run" ? t("agentRoute.thisAgentHasActiveConversationsFinishOrStopThem") : t("agentSettings.profileError")); }
    finally { if (!controller.signal.aborted) setBusy(false); }
  }
  return <div className="agentSettingsLayout">
    <header><h1>{t("agentSettings.agents")}</h1><button className="shPrimaryButton" type="button" disabled={busy} onClick={() => navigateSettings(`${base}?new=1`)}><Plus aria-hidden="true" />{t("agentCreateRoute.createAgent")}</button></header>
    <nav className="agentSettingsList" aria-label={t("agentSettings.privateAgents")}>
      {agents.map(agent => <Link className={selected?.id === agent.id ? "isActive" : ""} aria-current={selected?.id === agent.id ? "page" : undefined} to={agentSettingsPath(workspace.id, agent.id)} state={location.state} key={agent.id}><AgentMark agent={agent} /><span>{agent.name}</span></Link>)}
    </nav>
    {selected ? <article className="agentSettingsDetail">
      <header><AgentMark agent={selected} /><div><h2>{selected.name}</h2><p>{selected.description}</p></div></header>
      <AgentModelSettings key={selected.id} agentId={selected.id} onRemove={remove} renderProfileActions={requestRemove => <div className="agentSettingsActions"><button type="button" disabled={busy} onClick={() => setEditing(true)}><Pencil aria-hidden="true" />{t("agentRoute.editAgent")}</button><button className="isDanger" type="button" disabled={busy} onClick={requestRemove}><Trash2 aria-hidden="true" />{t("agentRoute.moveToTrash")}</button><Link to={`/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(selected.id)}`}>{t("agentRoute.openConversation")}</Link></div>} />
    </article> : <p>{t(agents.length ? "agentSettings.selectAgent" : "workspaceHomeRoute.noPrivateAgentsAreAvailableInThisWorkspaceYet")}</p>}
    {error && !editorOpen ? <p role="alert">{error}</p> : null}
    {editorOpen ? createPortal(<div className="agentSettingsEditorLayer"><AgentEditorModal agent={creating ? { name: "", description: "", instructions: "", avatarKind: "centaeris" } : selected} heading={t(creating ? "agentCreateRoute.createPrivateAgent" : "agentRoute.editAgent")} submitLabel={t(creating ? "agentCreateRoute.createAgent" : "agentRoute.saveChanges")} busy={busy} error={error} onClose={closeEditor} onSave={saveProfile} /></div>, document.body) : null}
  </div>;
}
