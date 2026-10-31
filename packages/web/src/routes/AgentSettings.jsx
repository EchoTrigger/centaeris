import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Link, useLocation, useNavigate, useRevalidator, useRouteLoaderData } from "react-router";
import { Plus, Pencil, Trash2, Eye } from "lucide-react";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";
import { AgentMark } from "../shell/AgentMark";
import { AgentDocumentIdentity } from "../shell/useAgentDocumentIdentity";
import { AgentEditorModal } from "../shell/AgentEditorModal";
import { agentSettingsPath, agentSettingsSelection } from "../agent-chat/agentSettingsNavigation";
import AgentModelSettings from "./AgentModelSettings";
import "./agent-settings.css";

export default function AgentSettings({ onEditorOpenChange }) {
  const { t } = useTranslation();
  const { workspace, agents } = useRouteLoaderData("workspace");
  const location = useLocation(); const navigate = useNavigate(); const revalidator = useRevalidator();
  const promotedAgentsRef = useRef(new Map());
  const [, setPromotionRevision] = useState(0);
  function recordPromotion(agent) { promotedAgentsRef.current.set(agent.id, { definitionId: agent.definitionId, definitionVersionId: agent.definitionVersionId }); setPromotionRevision(value => value + 1); }
  const selection = agentSettingsSelection(location.search, agents.map(agent => agent.id));
  const selectedSource = selection.kind === "agent" ? agents.find(agent => agent.id === selection.agentId) : null;
  const selected = selectedSource?.definitionId === null && promotedAgentsRef.current.has(selectedSource.id) ? { ...selectedSource, ...promotedAgentsRef.current.get(selectedSource.id) } : selectedSource;
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const mutationControllerRef = useRef(null);
  useEffect(() => () => mutationControllerRef.current?.abort(), []);
  const canEdit = ["owner", "admin"].includes(workspace.role);
  const creating = selection.kind === "create" && canEdit;
  const editorOpen = creating || Boolean(selected && editing);
  useEffect(() => { onEditorOpenChange(editorOpen); return () => onEditorOpenChange(false); }, [editorOpen, onEditorOpenChange]);
  const base = agentSettingsPath(workspace.id);
  const navigateSettings = path => navigate(path, { state: location.state });
  const closeEditor = () => { if (creating) navigateSettings(base); else setEditing(false); };
  async function saveProfile(value) {
    if (busy || !canEdit) return;
    setBusy(true); setError("");
    const controller = new AbortController(); mutationControllerRef.current = controller;
    try {
      if (!creating && selected.definitionId === null && !selected.isBusinessRoot) {
        const promoted = await apiJson(`/api/agents/${encodeURIComponent(selected.id)}/promote`, { ...jsonOptions("POST", {}), signal: controller.signal });
        if (!promoted?.agent?.definitionId || promoted.agent.id !== selected.id || promoted.agent.workspaceId !== workspace.id) throw new Error("agent_settings_response_invalid");
        recordPromotion(promoted.agent);
      }
      const result = await apiJson(creating ? `/api/workspaces/${encodeURIComponent(workspace.id)}/agents` : `/api/agents/${encodeURIComponent(selected.id)}`, { ...jsonOptions(creating ? "POST" : "PATCH", value), signal: controller.signal });
      if (controller.signal.aborted) return;
      if (!result?.agent?.id || result.agent.workspaceId !== workspace.id || (!creating && result.agent.id !== selected.id)) throw new Error("agent_settings_response_invalid");
      await revalidator.revalidate();
      if (controller.signal.aborted) return;
      setEditing(false);
      navigateSettings(agentSettingsPath(workspace.id, result.agent.id));
    } catch (failure) { if (!controller.signal.aborted) setError(t(["agent_avatar_invalid", "agent_avatar_too_large", "agent_avatar_dimensions_invalid"].includes(failure.message) ? "agentEditorModal.avatarInvalid" : "agentSettings.profileError")); }
    finally { if (!controller.signal.aborted) setBusy(false); }
  }
  async function remove() {
    if (busy || !selected || !canEdit) return;
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
    <AgentDocumentIdentity agent={selected} />
    <header><h1>{t("agentSettings.agents")}</h1>{canEdit ? <button className="shPrimaryButton" type="button" disabled={busy} onClick={() => navigateSettings(`${base}?new=1`)}><Plus aria-hidden="true" />{t("agentCreateRoute.createAgent")}</button> : null}</header>
    <nav className="agentSettingsList" aria-label={t("agentSettings.workspaceAgents")}>
      {agents.map(agent => <Link className={selected?.id === agent.id ? "isActive" : ""} aria-current={selected?.id === agent.id ? "page" : undefined} to={agentSettingsPath(workspace.id, agent.id)} state={location.state} key={agent.id}><AgentMark agent={agent} /><span>{agent.name}</span></Link>)}
    </nav>
    {selected ? <article className="agentSettingsDetail">
      <header><AgentMark agent={selected} /><div><h2>{selected.name}</h2><p>{selected.description}</p></div></header>
      <AgentModelSettings key={selected.id} agentId={selected.id} definitionId={selected.definitionId} onPromoted={recordPromotion} onRemove={remove} renderProfileActions={requestRemove => <div className="agentSettingsActions"><button type="button" disabled={busy} onClick={() => setEditing(true)}>{canEdit ? <Pencil aria-hidden="true" /> : <Eye aria-hidden="true" />}{t(canEdit ? "agentRoute.editAgent" : "agentSettings.viewAgent")}</button>{canEdit ? <button className="isDanger" type="button" disabled={busy} onClick={requestRemove}><Trash2 aria-hidden="true" />{t("agentRoute.moveToTrash")}</button> : null}<Link to={`/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(selected.id)}`}>{t("agentRoute.openConversation")}</Link></div>} />
    </article> : <p>{t(agents.length ? "agentSettings.selectAgent" : "workspaceHomeRoute.noAgentsAreAvailableInThisWorkspaceYet")}</p>}
    {error && !editorOpen ? <p role="alert">{error}</p> : null}
    {editorOpen ? createPortal(<div className="agentSettingsEditorLayer"><AgentEditorModal agent={creating ? { name: "", description: "", instructions: "", avatarKind: "centaeris" } : selected} readOnly={!canEdit} shared={creating || Boolean(selected?.definitionId)} heading={t(creating ? "agentCreateRoute.createAgent" : canEdit ? "agentRoute.editAgent" : "agentSettings.viewAgent")} submitLabel={t(creating ? "agentCreateRoute.createAgent" : "agentRoute.saveChanges")} busy={busy} error={error} onClose={closeEditor} onSave={saveProfile} /></div>, document.body) : null}
  </div>;
}
