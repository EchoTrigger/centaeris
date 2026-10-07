import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useBlocker } from "react-router";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";
import { ModelPicker, ThinkingPicker, thinkingModeLabels } from "../components/ModelSelectionPickers";
import { createAgentModelSettingsClient, groupAgentModels, parseAgentModelCatalog } from "../agent-chat/agentModelSettings";

export default function AgentModelSettings({ agentId, onRemove, renderProfileActions }) {
  const { t } = useTranslation();
  const client = useMemo(() => createAgentModelSettingsClient(agentId, apiJson), [agentId]);
  const [settings, setSettings] = useState(null);
  const [catalog, setCatalog] = useState([]);
  const [draft, setDraft] = useState({ modelConfigRef: null, thinkingMode: null, defaultRequested: false });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const controllerRef = useRef(null);
  const accept = useCallback(value => {
    setSettings(value);
    setDraft({ modelConfigRef: value.modelConfigRef, thinkingMode: value.thinkingMode, defaultRequested: false });
    setConfirmingDelete(false);
  }, []);
  const load = useCallback(async () => {
    controllerRef.current?.abort();
    const controller = new AbortController(); controllerRef.current = controller;
    setLoading(true); setError(""); setSaved(false);
    try {
      const [value, models] = await Promise.all([client.load(controller.signal), apiJson("/api/models", { signal: controller.signal }).then(parseAgentModelCatalog)]);
      if (!controller.signal.aborted) { accept(value); setCatalog(models); }
    } catch { if (!controller.signal.aborted) setError("loadError"); }
    finally { if (!controller.signal.aborted) setLoading(false); }
  }, [client, accept]);
  useEffect(() => { void load(); return () => controllerRef.current?.abort(); }, [load]);
  const currentModel = catalog.find(model => model.id === draft.modelConfigRef);
  const groups = useMemo(() => groupAgentModels(catalog, t("appRoute.models")), [catalog, t]);
  const dirty = settings && (draft.defaultRequested || draft.modelConfigRef !== settings.modelConfigRef || draft.thinkingMode !== settings.thinkingMode);
  const dirtyRef = useRef(false); dirtyRef.current = Boolean(dirty);
  const blocker = useBlocker(({ currentLocation, nextLocation }) => dirtyRef.current
    && (currentLocation.pathname !== nextLocation.pathname || currentLocation.search !== nextLocation.search));
  const continueEditingRef = useRef(null);
  useEffect(() => {
    if (!dirty && blocker.state === "blocked") blocker.reset();
    else if (blocker.state === "blocked" || confirmingDelete) continueEditingRef.current?.focus();
  }, [dirty, blocker, confirmingDelete]);
  function resetDraft() {
    dirtyRef.current = false; accept(settings); setError(""); setSaved(false);
    if (blocker.state === "blocked") blocker.reset();
  }
  function requestRemove() {
    if (!dirty) { void onRemove(); return; }
    if (blocker.state === "blocked") blocker.reset();
    setConfirmingDelete(true);
  }
  const valid = draft.modelConfigRef === null || Boolean(currentModel && (draft.thinkingMode === null || currentModel.thinkingModes.includes(draft.thinkingMode)));
  const update = patch => { setDraft(previous => ({ ...previous, ...patch })); setSaved(false); setError(""); };
  async function save() {
    if (saving || !dirty || !valid) return;
    setSaving(true); setSaved(false); setError("");
    const controller = new AbortController(); controllerRef.current?.abort(); controllerRef.current = controller;
    try {
      const value = await client.update(draft.modelConfigRef, draft.thinkingMode, controller.signal);
      if (!controller.signal.aborted) { accept(value); setSaved(true); }
    } catch (failure) {
      if (!controller.signal.aborted) {
        const key = failure.message === "model_thinking_mode_unsupported" ? "effortUnavailable" : failure.message === "agent_model_not_available" ? "unavailable" : "saveError";
        setError(key);
      }
    } finally { if (!controller.signal.aborted) setSaving(false); }
  }
  const effort = draft.defaultRequested ? t("agentSettings.modelDefault") : draft.thinkingMode === null ? t("agentSettings.noEffort") : thinkingModeLabels(t)[draft.thinkingMode] || draft.thinkingMode;
  return <>{renderProfileActions?.(requestRemove)}<section className="agentSettingsModel" aria-label={t("agentSettings.model")}>
    <h3>{t("agentSettings.model")}</h3>
    {loading ? <p role="status">{t("agentChat.loading")}</p> : settings ? <>
      <p className="agentSettingsStatus">{t(`agentSettings.${settings.status}`)}</p>
      <div className="agentSettingsModelControls">
        <div><span>{t("appRoute.aiModel")}</span><ModelPicker groups={groups} model={currentModel} value={draft.modelConfigRef || ""} disabled={saving || !groups.length} onChange={id => update({ modelConfigRef: id, thinkingMode: null, defaultRequested: true })} /></div>
        <div><span>{t("appRoute.reasoningEffort")}: {effort}</span><ThinkingPicker model={currentModel} value={draft.thinkingMode || ""} disabled={saving || !currentModel?.thinkingModes.length} onChange={mode => update({ thinkingMode: mode, defaultRequested: false })} /></div>
      </div>
      {draft.modelConfigRef && !currentModel && settings.status !== "unavailable" ? <p role="alert">{t("agentSettings.unavailable")}</p> : null}
      <p>{t("agentSettings.futureRuns")}</p>
      <div className="agentSettingsActions">
        <button type="button" disabled={saving || !currentModel} onClick={() => update({ thinkingMode: null, defaultRequested: true })}>{t("agentSettings.useDefault")}</button>
        <button type="button" disabled={saving || draft.modelConfigRef === null} onClick={() => update({ modelConfigRef: null, thinkingMode: null, defaultRequested: false })}>{t("agentSettings.clearModel")}</button>
        <button type="button" disabled={saving || !dirty} onClick={resetDraft}>{t("agentEditorModal.reset")}</button>
        <button className="shPrimaryButton" type="button" disabled={saving || !dirty || !valid} onClick={() => void save()}>{saving ? t("modelSettings.saving") : t("agentRoute.saveChanges")}</button>
      </div>
    </> : null}
    {error ? <div role="alert"><p>{t(`agentSettings.${error}`)}</p>{!settings ? <button type="button" onClick={() => void load()}>{t("agentChat.retry")}</button> : null}</div> : null}
    {saved ? <p role="status">{t("agentSettings.saved")}</p> : null}
    {confirmingDelete ? <div className="agentSettingsActions" role="alert">
      <span>{t("agentSettings.modelDiscardHint")}</span>
      <button ref={continueEditingRef} type="button" onClick={() => setConfirmingDelete(false)}>{t("agentEditorModal.continueEditing")}</button>
      <button className="isDanger" type="button" onClick={() => { resetDraft(); void onRemove(); }}>{t("agentSettings.discardAndTrash")}</button>
    </div> : null}
    {blocker.state === "blocked" ? <div className="agentSettingsActions" role="alert">
      <span>{t("agentEditorModal.discardUnsavedChanges")} {t("agentSettings.modelDiscardHint")}</span>
      <button ref={continueEditingRef} type="button" onClick={() => blocker.reset()}>{t("agentEditorModal.continueEditing")}</button>
      <button className="isDanger" type="button" onClick={() => blocker.proceed()}>{t("agentEditorModal.discardChanges")}</button>
    </div> : null}
  </section></>;
}
