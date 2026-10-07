
import { useTranslation } from "../i18n";
import { useEffect, useMemo, useState } from "react";
import { ArrowLeft, Eye, EyeOff, KeyRound, Plus } from "lucide-react";
import { apiJson as api } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { ProviderLogo } from "../components/ProviderLogo";
import { localizedModelError, modelErrorText } from "../modelErrors";

const API_OPTIONS = ["openai-completions", "openai-responses", "anthropic-messages"];
const PROVIDER_TIERS = [
  { id: "direct_api", label: "Direct API" },
  { id: "coding_plan", label: "Coding Plans" },
  { id: "token_plan", label: "Token Plans" },
];
let nextDraftId = 1;

function draftId(kind) {
  return `${kind}_${nextDraftId++}`;
}

function providerValues(provider) {
  return {
    displayName: provider.displayName,
    nameInput: provider.displayName,
    api: provider.api,
    apiBase: provider.apiBase,
    secret: "",
  };
}

function modelValues(model) {
  return {
    displayName: model.displayName || "",
    modelName: model.modelName || "",
    apiOverride: model.apiOverride || "",
    contextTokens: String(model.contextTokens || 128000),
    maxOutputTokens: String(model.maxOutputTokens || 16384),
    thinkingMode: model.thinkingMode || "",
    thinkingModes: (model.thinkingModes || []).join(", "),
    enabled: model.enabled !== false,
  };
}

function positiveInteger(value, label) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number <= 0) throw new Error(`${label}_invalid`);
  return number;
}

function testSummary(result, t) {
  const status = result.httpStatus ? `HTTP ${result.httpStatus}` : result.ok ? "OK" : "HTTP error";
  const detail = result.ok ? result.outputPreview || "OK"
    : localizedModelError(result.errorKeyword || "model_adapter_failed", t) ?? result.errorKeyword;
  return `${status} · ${result.latencyMs}ms · ${detail}`;
}

export default function ModelSettings({ onClose, onModelsChanged }) {
  const { t } = useTranslation();
  const errorText = error => modelErrorText(error, t);
  const [providers, setProviders] = useState([]);
  const [models, setModels] = useState([]);
  const [templates, setTemplates] = useState([]);
  const [draftProviders, setDraftProviders] = useState([]);
  const [draftModels, setDraftModels] = useState([]);
  const [providerForms, setProviderForms] = useState({});
  const [modelForms, setModelForms] = useState({});
  const [selection, setSelection] = useState({ kind: "empty" });
  const [pickerOpen, setPickerOpen] = useState(false);
  const [revealedProviderId, setRevealedProviderId] = useState("");
  const [busyAction, setBusyAction] = useState("");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [modelTest, setModelTest] = useState(null);
  const [confirmAction, setConfirmAction] = useState("");

  const allProviders = useMemo(
    () => [...providers, ...draftProviders],
    [providers, draftProviders],
  );
  const allModels = useMemo(() => [...models, ...draftModels], [models, draftModels]);
  const selectedProviderId = selection.providerId || "";
  const selectedProvider = allProviders.find((provider) => provider.id === selectedProviderId) || null;
  const availableTemplates = templates.filter(
    (template) => !allProviders.some((provider) => provider.templateId === template.id),
  );
  const selectedModel = selection.kind === "model"
    ? allModels.find((model) => model.id === selection.modelId) || null
    : null;
  const selectedProviderForm = selectedProvider ? providerForms[selectedProvider.id] : null;
  const providerKeyInputId = selectedProvider ? `model-provider-key-${selectedProvider.id}` : undefined;
  const selectedModelForm = selectedModel ? modelForms[selectedModel.id] : null;
  const selectedTest = modelTest?.modelId === selectedModel?.id ? modelTest : null;
  async function load() {
    const [providersResult, modelsResult, templatesResult] = await Promise.all([
      api("/api/admin/model-providers"),
      api("/api/admin/models"),
      api("/api/admin/model-provider-templates"),
    ]);
    setProviders(providersResult.providers);
    setModels(modelsResult.models);
    setTemplates(templatesResult.templates);
    setProviderForms((current) => ({
      ...Object.fromEntries(providersResult.providers.map((provider) => [provider.id, providerValues(provider)])),
      ...Object.fromEntries(draftProviders.map((provider) => [provider.id, current[provider.id]]).filter(([, value]) => value)),
    }));
    setModelForms((current) => ({
      ...Object.fromEntries(modelsResult.models.map((model) => [model.id, modelValues(model)])),
      ...Object.fromEntries(draftModels.map((model) => [model.id, current[model.id]]).filter(([, value]) => value)),
    }));
    setSelection((current) => current.kind === "empty" && providersResult.providers[0]
      ? { kind: "provider", providerId: providersResult.providers[0].id }
      : current);
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: Initial catalog loading is mount-only; successful mutations refresh explicitly.
  useEffect(() => {
    setBusyAction("load");
    load().catch((loadError) => setError(errorText(loadError))).finally(() => setBusyAction(""));
  }, []);

  useEffect(() => {
    if (!pickerOpen) return undefined;
    const closePicker = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopImmediatePropagation();
      setPickerOpen(false);
    };
    window.addEventListener("keydown", closePicker, true);
    return () => window.removeEventListener("keydown", closePicker, true);
  }, [pickerOpen]);

  function updateProviderForm(providerId, patch) {
    setProviderForms((current) => ({ ...current, [providerId]: { ...current[providerId], ...patch } }));
  }

  function updateModelForm(modelId, patch) {
    setModelForms((current) => ({ ...current, [modelId]: { ...current[modelId], ...patch } }));
  }

  function openPicker() {
    setPickerOpen(true);
  }

  function addCustomProvider() {
    const id = draftId("draft_provider");
    const provider = { id, displayName: "new-provider", api: "openai-completions", apiBase: "", credentialVersion: 0, isDraft: true };
    setDraftProviders((current) => [...current, provider]);
    setProviderForms((current) => ({ ...current, [id]: providerValues(provider) }));
    setSelection({ kind: "provider", providerId: id });
    setPickerOpen(false);
    setMessage("");
  }

  function addTemplateProvider(template) {
    const id = draftId("draft_provider");
    const provider = { ...template, id, credentialVersion: 0, isDraft: true, templateId: template.id };
    setDraftProviders((current) => [...current, provider]);
    setProviderForms((current) => ({ ...current, [id]: providerValues(provider) }));
    setSelection({ kind: "provider", providerId: id });
    setPickerOpen(false);
    setMessage("");
  }

  async function renameProvider() {
    const name = selectedProviderForm?.nameInput.trim();
    if (!name) {
      setError("provider_name_required");
      return;
    }
    if (selectedProvider.isDraft) {
      updateProviderForm(selectedProvider.id, { displayName: name, nameInput: name });
      setError("");
      setMessage("Renamed");
      return;
    }
    if (name === selectedProvider.displayName) {
      setMessage(t("interface.saved"));
      return;
    }
    setBusyAction("rename");
    setError("");
    setMessage("");
    try {
      const saved = (await api(`/api/admin/model-providers/${selectedProvider.id}`, {
        method: "PATCH",
        body: JSON.stringify({ displayName: name }),
      })).provider;
      setProviderForms((current) => ({ ...current, [saved.id]: providerValues(saved) }));
      await load();
      await onModelsChanged();
      setMessage(t("interface.saved"));
    } catch (renameError) {
      setError(errorText(renameError));
    } finally {
      setBusyAction("");
    }
  }

  function addModel(providerId) {
    const id = draftId("draft_model");
    const model = {
      id,
      providerId,
      displayName: "",
      modelName: "",
      apiOverride: null,
      contextTokens: 128000,
      maxOutputTokens: 16384,
      thinkingMode: null,
      thinkingModes: [],
      enabled: true,
      isDraft: true,
    };
    setDraftModels((current) => [...current, model]);
    setModelForms((current) => ({ ...current, [id]: modelValues(model) }));
    setSelection({ kind: "model", providerId, modelId: id });
    setMessage("");
  }

  async function saveProvider() {
    if (!selectedProvider || !selectedProviderForm) return;
    const form = selectedProviderForm;
    setBusyAction("save-provider");
    setError("");
    setMessage("");
    try {
      let saved;
      if (selectedProvider.isDraft) {
        if (!form.secret.trim()) throw new Error("provider_api_key_required");
        saved = selectedProvider.templateId
          ? (await api(`/api/admin/model-provider-templates/${selectedProvider.templateId}/instantiate`, {
            method: "POST",
            body: JSON.stringify({ secret: form.secret.trim() }),
          })).provider
          : (await api("/api/admin/model-providers", {
            method: "POST",
            body: JSON.stringify({
              displayName: form.displayName.trim(),
              api: form.api,
              apiBase: form.apiBase.trim(),
              secret: form.secret.trim(),
            }),
          })).provider;
        setDraftProviders((current) => current.filter((provider) => provider.id !== selectedProvider.id));
        setDraftModels((current) => current.map((model) => model.providerId === selectedProvider.id ? { ...model, providerId: saved.id } : model));
        setProviderForms((current) => {
          const { [selectedProvider.id]: removed, ...rest } = current;
          return { ...rest, [saved.id]: providerValues(saved) };
        });
        setSelection({ kind: "provider", providerId: saved.id });
      } else {
        const patch = {};
        if (form.displayName !== selectedProvider.displayName) patch.displayName = form.displayName.trim();
        if (form.api !== selectedProvider.api) patch.api = form.api;
        if (form.apiBase !== selectedProvider.apiBase) patch.apiBase = form.apiBase.trim();
        if (Object.keys(patch).length) {
          saved = (await api(`/api/admin/model-providers/${selectedProvider.id}`, {
            method: "PATCH",
            body: JSON.stringify(patch),
          })).provider;
        }
        if (form.secret.trim()) {
          saved = (await api(`/api/admin/model-providers/${selectedProvider.id}/credential/rotate`, {
            method: "POST",
            body: JSON.stringify({ secret: form.secret.trim() }),
          })).provider;
        }
        if (!saved) saved = selectedProvider;
        setProviderForms((current) => ({ ...current, [saved.id]: providerValues(saved) }));
      }
      await load();
      await onModelsChanged();
      setMessage(t("interface.saved"));
    } catch (saveError) {
      setError(errorText(saveError));
    } finally {
      setBusyAction("");
    }
  }

  async function saveModel() {
    if (!selectedModel || !selectedModelForm) return;
    if (selectedProvider?.isDraft) {
      setError("save_provider_before_model");
      return;
    }
    setBusyAction("save-model");
    setError("");
    setMessage("");
    try {
      const contextTokens = positiveInteger(selectedModelForm.contextTokens, "context_tokens");
      const maxOutputTokens = positiveInteger(selectedModelForm.maxOutputTokens, "max_output_tokens");
      if (maxOutputTokens >= contextTokens) throw new Error("max_output_tokens_must_be_smaller_than_context_tokens");
      const thinkingModes = selectedModelForm.thinkingModes.split(",").map((value) => value.trim()).filter(Boolean);
      if (new Set(thinkingModes).size !== thinkingModes.length) throw new Error("thinking_modes_must_be_unique");
      const thinkingMode = selectedModelForm.thinkingMode.trim() || null;
      if (thinkingMode && !thinkingModes.includes(thinkingMode)) throw new Error("thinking_mode_must_be_supported");
      const payload = {
        displayName: selectedModelForm.displayName.trim(),
        providerId: selectedModel.providerId,
        modelName: selectedModelForm.modelName.trim(),
        apiOverride: selectedModelForm.apiOverride || null,
        contextTokens,
        maxOutputTokens,
        thinkingMode,
        thinkingModes,
        enabled: selectedModelForm.enabled,
      };
      if (!payload.modelName) throw new Error("model_id_required");
      const saved = selectedModel.isDraft
        ? (await api("/api/admin/models", { method: "POST", body: JSON.stringify(payload) })).model
        : (await api(`/api/admin/models/${selectedModel.id}`, { method: "PATCH", body: JSON.stringify(payload) })).model;
      setDraftModels((current) => current.filter((model) => model.id !== selectedModel.id));
      setModelForms((current) => {
        const { [selectedModel.id]: removed, ...rest } = current;
        return { ...rest, [saved.id]: modelValues(saved) };
      });
      setSelection({ kind: "model", providerId: saved.providerId, modelId: saved.id });
      await load();
      await onModelsChanged();
      setMessage(t("interface.saved"));
    } catch (saveError) {
      setError(errorText(saveError));
    } finally {
      setBusyAction("");
    }
  }

  async function removeProvider() {
    if (!selectedProvider) return;
    setBusyAction("remove-provider");
    setError("");
    try {
      if (selectedProvider.isDraft) {
        setDraftProviders((current) => current.filter((provider) => provider.id !== selectedProvider.id));
        setDraftModels((current) => current.filter((model) => model.providerId !== selectedProvider.id));
      } else {
        await api(`/api/admin/model-providers/${selectedProvider.id}`, { method: "DELETE" });
        await load();
        await onModelsChanged();
      }
      setSelection({ kind: "empty" });
      setConfirmAction("");
      setMessage("Removed");
    } catch (removeError) {
      setError(errorText(removeError));
    } finally {
      setBusyAction("");
    }
  }

  async function removeModel() {
    if (!selectedModel) return;
    setBusyAction("remove-model");
    setError("");
    try {
      if (selectedModel.isDraft) {
        setDraftModels((current) => current.filter((model) => model.id !== selectedModel.id));
      } else {
        await api(`/api/admin/models/${selectedModel.id}`, { method: "DELETE" });
        await load();
        await onModelsChanged();
      }
      setSelection({ kind: "provider", providerId: selectedModel.providerId });
      setConfirmAction("");
      setMessage("Removed");
    } catch (removeError) {
      setError(errorText(removeError));
    } finally {
      setBusyAction("");
    }
  }

  async function testModel() {
    if (!selectedModel || selectedModel.isDraft) return;
    setBusyAction("test-model");
    setError("");
    setModelTest(null);
    try {
      const result = await api(`/api/admin/models/${selectedModel.id}/test`, { method: "POST", body: "{}" });
      setModelTest({ ...result, modelId: selectedModel.id });
    } catch (testError) {
      setError(errorText(testError));
    } finally {
      setBusyAction("");
    }
  }

  return <div className="workspaceModelsLayout">
    <aside className="workspaceModelsSidebar" aria-label={t("interface.modelProviders")}>
      {allProviders.map((provider) => {
        const form = providerForms[provider.id];
        const providerModels = allModels.filter((model) => model.providerId === provider.id);
        const providerSelected = selectedProviderId === provider.id;
        return <div className="workspaceModelsProviderGroup" key={provider.id}>
          <button type="button" className={providerSelected && selection.kind === "provider" ? "is-active" : ""} onClick={() => { setSelection({ kind: "provider", providerId: provider.id }); setPickerOpen(false); }}>
            <span className="workspaceModelsProviderIdentity"><ProviderLogo svg={templates.find((template) => template.id === provider.templateId)?.logoSvg} name={form?.displayName || provider.displayName} />{form?.displayName || provider.displayName}</span>{provider.credentialVersion ? <i aria-label={t("modelSettings.configured")} /> : null}
          </button>
          {providerSelected && !provider.templateId ? <div className="workspaceModelsTreeModels">
            {providerModels.map((model) => <button type="button" className={selection.kind === "model" && selection.modelId === model.id ? "is-selected" : ""} key={model.id} onClick={() => { setSelection({ kind: "model", providerId: provider.id, modelId: model.id }); setPickerOpen(false); }}>{modelForms[model.id]?.modelName || t("interface.newModel")}</button>)}
            <button type="button" className="workspaceModelsAddModel" onClick={() => addModel(provider.id)}><Plus aria-hidden="true" />{t("models.addModel")}</button>
          </div> : null}
        </div>;
      })}
      <button type="button" className="workspaceModelsAddProvider" onClick={openPicker}><Plus aria-hidden="true" />{t("interface.addProvider")}</button>
    </aside>

    <section className="workspaceModelsEditor">
      {pickerOpen ? <section className="workspaceModelsPicker" aria-label={t("interface.addProvider")}>
        <header className="workspaceModelsPickerHeader"><button type="button" aria-label={t("interface.backToModels")} onClick={() => setPickerOpen(false)}><ArrowLeft aria-hidden="true" /></button><strong>{t("interface.addProvider")}</strong></header>
        <div className="workspaceModelsPickerContent"><section><div>{t("interface.custom")}</div><button type="button" className="workspaceModelsCustomCard" onClick={addCustomProvider}><span><strong>{t("interface.custom")}</strong><small>OpenAI / Anthropic</small></span><Plus aria-hidden="true" /></button></section>{PROVIDER_TIERS.map((tier) => {
          const tierTemplates = availableTemplates.filter((template) => template.tier === tier.id);
          return tierTemplates.length ? <section key={tier.id}><div>{tier.label}</div><div className="workspaceModelsPickerGrid">{tierTemplates.map((template) => <button type="button" key={template.id} onClick={() => addTemplateProvider(template)}><span className="workspaceModelsProviderIdentity"><ProviderLogo svg={template.logoSvg} name={template.displayName} /><strong>{template.displayName}</strong></span><span>{t("models.count", { count: template.models.length })}</span></button>)}</div></section> : null;
        })}</div>
      </section> : <>
      <div className="workspaceModelsEditorScroll">
        {error ? <div className="workspaceModelsMessage is-error" role="alert">{error}</div> : null}
        {message ? <div className="workspaceModelsMessage" role="status">{message}</div> : null}
        {selection.kind === "empty" ? <div className="workspaceModelsEmpty"><strong>{t("interface.noProviders")}</strong><span>{t("interface.addAProviderOrCustomHttpsEndpoint")}</span><button type="button" onClick={openPicker}><Plus aria-hidden="true" />{t("interface.addProvider")}</button></div> : null}

        {selection.kind === "provider" && selectedProvider && selectedProviderForm ? selectedProvider.templateId ? <section className="workspaceModelsSection workspaceModelsPreset">
          <div className="workspaceModelsSectionHeader"><span>{t("interface.apiKey")}</span><small className={selectedProvider.credentialVersion ? "is-configured" : ""}><i aria-hidden="true" />{selectedProvider.credentialVersion ? t("modelSettings.configured") : t("modelSettings.notConfiguredYet")}</small></div>
          <div className="workspaceModelsField">
            <div className="workspaceModelsKeyLine"><div className="workspaceModelsKeyInput"><KeyRound aria-hidden="true" /><input id={providerKeyInputId} name="providerApiKey" aria-label={t("interface.apiKey")} type={revealedProviderId === selectedProvider.id ? "text" : "password"} autoComplete="new-password" spellCheck={false} value={selectedProviderForm.secret} onChange={(event) => updateProviderForm(selectedProvider.id, { secret: event.target.value })} placeholder={selectedProvider.credentialVersion ? t("modelSettings.enterANewKeyToReplaceIt") : t("modelSettings.enterApiKey")} /><button type="button" aria-label={t("modelSettings.showOrHideApiKey")} onClick={() => setRevealedProviderId((current) => current === selectedProvider.id ? "" : selectedProvider.id)}>{revealedProviderId === selectedProvider.id ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</button></div><button type="button" className="workspaceModelsInlineSave" disabled={Boolean(busyAction) || !selectedProviderForm.secret.trim()} onClick={() => void saveProvider()}>{busyAction === "save-provider" ? t("modelSettings.saving") : t("modelSettings.save")}</button></div>
          </div>
          {selectedProvider.credentialVersion ? <button type="button" className="workspaceModelsDanger workspaceModelsDisconnect" onClick={() => setConfirmAction("provider")} disabled={Boolean(busyAction)}>{t("modelSettings.disconnect")}</button> : null}
        </section> : <section className="workspaceModelsSection">
          <div className="workspaceModelsSectionHeader"><span>{t("interface.customProvider")}</span><button type="button" className="workspaceModelsDanger" onClick={() => setConfirmAction("provider")} disabled={Boolean(busyAction)}>{t("globalPluginSettings.remove")}</button></div>
          <label className="workspaceModelsField"><span>{t("modelSettings.providerName")}</span><input value={selectedProviderForm.nameInput} onChange={(event) => updateProviderForm(selectedProvider.id, { nameInput: event.target.value })} /></label>
          <button type="button" className="workspaceModelsRename" disabled={Boolean(busyAction)} onClick={() => void renameProvider()}>{busyAction === "rename" ? t("modelSettings.saving") : t("appRoute.rename")}</button>
          <label className="workspaceModelsField"><span>{t("interface.baseUrl")}</span><input type="url" value={selectedProviderForm.apiBase} onChange={(event) => updateProviderForm(selectedProvider.id, { apiBase: event.target.value })} placeholder="https://api.example.com/v1" /></label>
          <label className="workspaceModelsField"><span>{t("modelSettings.apiProtocol")}</span><select value={selectedProviderForm.api} onChange={(event) => updateProviderForm(selectedProvider.id, { api: event.target.value })}>{API_OPTIONS.map((api) => <option key={api} value={api}>{api}</option>)}</select></label>
          <section className="workspaceModelsKeySection">
            <div className="workspaceModelsSectionHeader"><span>{t("interface.apiKey")}</span><small className={selectedProvider.credentialVersion ? "is-configured" : ""}>{selectedProvider.credentialVersion ? t("modelSettings.configured") : t("modelSettings.notConfiguredYet")}</small></div>
            <div className="workspaceModelsKeyInput"><KeyRound aria-hidden="true" /><input id={providerKeyInputId} name="providerApiKey" aria-label={t("interface.apiKey")} type={revealedProviderId === selectedProvider.id ? "text" : "password"} autoComplete="new-password" spellCheck={false} value={selectedProviderForm.secret} onChange={(event) => updateProviderForm(selectedProvider.id, { secret: event.target.value })} placeholder={selectedProvider.credentialVersion ? t("modelSettings.enterANewKeyToReplaceIt") : t("modelSettings.enterApiKey")} /><button type="button" aria-label={t("modelSettings.showOrHideApiKey")} onClick={() => setRevealedProviderId((current) => current === selectedProvider.id ? "" : selectedProvider.id)}>{revealedProviderId === selectedProvider.id ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</button></div>
          </section>
        </section> : null}

        {selection.kind === "model" && selectedModel && selectedModelForm ? <section className="workspaceModelsSection">
          <div className="workspaceModelsSectionHeader"><span>{t("interface.model")}</span><span><button type="button" className="workspaceModelsTest" disabled={selectedModel.isDraft || Boolean(busyAction)} onClick={() => void testModel()}>{busyAction === "test-model" ? t("interface.testing") : t("interface.test")}</button><button type="button" className="workspaceModelsDanger" disabled={Boolean(busyAction)} onClick={() => setConfirmAction("model")}>{t("interface.remove")}</button></span></div>
          {selectedTest ? <div className={selectedTest.ok ? "workspaceModelsTestResult is-ok" : "workspaceModelsTestResult is-error"}>{testSummary(selectedTest, t)}</div> : null}
          <div className="workspaceModelsFormGrid">
            <label className="workspaceModelsField"><span>ID *</span><input value={selectedModelForm.modelName} onChange={(event) => updateModelForm(selectedModel.id, { modelName: event.target.value })} placeholder="model-id" /></label>
            <label className="workspaceModelsField"><span>{t("interface.name")}</span><input value={selectedModelForm.displayName} onChange={(event) => updateModelForm(selectedModel.id, { displayName: event.target.value })} placeholder={t("interface.displayName")} /></label>
          </div>
          <label className="workspaceModelsField"><span>{t("interface.apiOverride")}</span><select value={selectedModelForm.apiOverride} onChange={(event) => updateModelForm(selectedModel.id, { apiOverride: event.target.value })}><option value="">{t("interface.inheritNone")}</option>{API_OPTIONS.map((api) => <option key={api} value={api}>{api}</option>)}</select></label>
          <div className="workspaceModelsFormGrid"><label className="workspaceModelsField"><span>{t("interface.contextWindowTokens")}</span><input inputMode="numeric" value={selectedModelForm.contextTokens} onChange={(event) => updateModelForm(selectedModel.id, { contextTokens: event.target.value })} /></label><label className="workspaceModelsField"><span>{t("interface.maxOutputTokens")}</span><input inputMode="numeric" value={selectedModelForm.maxOutputTokens} onChange={(event) => updateModelForm(selectedModel.id, { maxOutputTokens: event.target.value })} /></label></div>
          <div className="workspaceModelsFormGrid"><label className="workspaceModelsField"><span>{t("interface.defaultThinkingEffort")}</span><input value={selectedModelForm.thinkingMode} onChange={(event) => updateModelForm(selectedModel.id, { thinkingMode: event.target.value })} placeholder={t("interface.highOptional")} /></label><label className="workspaceModelsField"><span>{t("interface.supportedEfforts")}</span><input value={selectedModelForm.thinkingModes} onChange={(event) => updateModelForm(selectedModel.id, { thinkingModes: event.target.value })} placeholder="low, medium, high" /></label></div>
        </section> : null}
      </div>
      {selectedProvider?.templateId && selection.kind === "provider" ? null : <footer className="workspaceModelsActions"><button type="button" onClick={onClose} disabled={Boolean(busyAction)}>{t("agentRunRow.cancel")}</button>{selection.kind !== "empty" ? <button type="button" className="is-primary" disabled={Boolean(busyAction)} onClick={() => void (selection.kind === "provider" ? saveProvider() : saveModel())}>{busyAction.startsWith("save") ? t("modelSettings.saving") : t("modelSettings.save")}</button> : null}</footer>}
      </>}
    </section>
    <ConfirmDialog
      open={Boolean(confirmAction)}
      title={confirmAction === "provider" ? (selectedProvider?.templateId ? t("modelSettings.disconnectProvider") : t("modelSettings.removeProvider")) : t("modelSettings.removeModel")}
      message={confirmAction === "provider"
        ? t("modelSettings.valueAndItsModelsWillNoLongerBeAvailable", { value1: selectedProviderForm?.displayName || selectedProvider?.displayName || t("modelSettings.provider") })
        : t("modelSettings.removeValue", { value1: selectedModelForm?.modelName || t("appRoute.models") })}
      confirmLabel={selectedProvider?.templateId ? t("modelSettings.disconnect") : t("globalPluginSettings.remove")}
      cancelLabel={t("agentRunRow.cancel")}
      busy={busyAction === "remove-provider" || busyAction === "remove-model"}
      onCancel={() => setConfirmAction("")}
      onConfirm={() => void (confirmAction === "provider" ? removeProvider() : removeModel())}
    />
  </div>;
}
