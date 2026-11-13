
import { useTranslation } from "../i18n";
import { useEffect, useMemo, useState } from "react";
import { ArrowLeft, ChevronRight, KeyRound, Plus } from "lucide-react";
import { ApiError, apiJson as api } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { ProviderLogo } from "../components/ProviderLogo";
import { ModelDefaults } from "../components/ModelDefaults";
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
  const status = result.httpStatus ? "HTTP " + result.httpStatus : "";
  const latency = Number.isFinite(result.latencyMs) ? result.latencyMs + "ms" : "";
  const error = result.ok ? "" : result.failureMessage
    ?? localizedModelError(result.errorKeyword || "model_adapter_failed", t) ?? t("modelErrors.failed");
  return [status, latency, error].filter(Boolean).join(" · ");
}

const MODEL_FORM_ERRORS = new Map([
  ["provider_api_key_required", "modelSettings.enterApiKey"],
  ["context_tokens_invalid", "modelSettings.contextLimitsInvalid"],
  ["max_output_tokens_invalid", "modelSettings.contextLimitsInvalid"],
  ["max_output_tokens_must_be_smaller_than_context_tokens", "modelSettings.contextLimitsInvalid"],
  ["thinking_modes_must_be_unique", "modelSettings.thinkingModesInvalid"],
  ["thinking_mode_must_be_supported", "modelSettings.thinkingModesInvalid"],
  ["model_id_required", "modelSettings.modelIdRequired"],
]);

export default function ModelSettings({ workspace, onModelsChanged }) {
  const { t } = useTranslation();
  const apiBase = `/api/workspaces/${encodeURIComponent(workspace.id)}`;
  const writable = item => item?.isDraft || (item?.scope === "workspace" && item?.writable === true);
  const errorText = error => {
    const key = MODEL_FORM_ERRORS.get(error?.message);
    return key ? t(key) : modelErrorText(error, t);
  };
  const [providers, setProviders] = useState([]);
  const [models, setModels] = useState([]);
  const [templates, setTemplates] = useState([]);
  const [draftProviders, setDraftProviders] = useState([]);
  const [draftModels, setDraftModels] = useState([]);
  const [providerForms, setProviderForms] = useState({});
  const [modelForms, setModelForms] = useState({});
  const [selection, setSelection] = useState({ kind: "empty" });
  const [pickerOpen, setPickerOpen] = useState(false);
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
  const providerModels = selectedProvider ? allModels.filter(model => model.providerId === selectedProvider.id) : [];
  async function load() {
    const [providersResult, modelsResult, templatesResult] = await Promise.all([
      api(`${apiBase}/model-providers`),
      api(`${apiBase}/models`),
      api(`${apiBase}/model-provider-templates`),
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

  function showProvider(providerId) {
    setSelection({ kind: "provider", providerId });
    setMessage("");
    setError("");
  }

  function back() {
    if (selection.kind === "model") setSelection({ kind: "provider", providerId: selectedProviderId });
    else setSelection({ kind: "empty" });
    setError("");
    setMessage("");
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
    if (!writable(selectedProvider)) return;
    const name = selectedProviderForm?.nameInput.trim();
    if (!name) {
      setError(t("modelSettings.providerNameRequired"));
      return;
    }
    if (selectedProvider.isDraft) {
      updateProviderForm(selectedProvider.id, { displayName: name, nameInput: name });
      setError("");
      setMessage(t("interface.saved"));
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
      const saved = (await api(`${apiBase}/model-providers/${selectedProvider.id}`, {
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
    if (!writable(allProviders.find(provider => provider.id === providerId))) return;
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
    if (!selectedProvider || !selectedProviderForm || !writable(selectedProvider)) return;
    const form = selectedProviderForm;
    setBusyAction("save-provider");
    setError("");
    setMessage("");
    try {
      let saved;
      if (selectedProvider.isDraft) {
        if (!form.secret.trim()) throw new Error("provider_api_key_required");
        saved = selectedProvider.templateId
          ? (await api(`${apiBase}/model-provider-templates/${selectedProvider.templateId}/instantiate`, {
            method: "POST",
            body: JSON.stringify({ secret: form.secret.trim() }),
          })).provider
          : (await api(`${apiBase}/model-providers`, {
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
          saved = (await api(`${apiBase}/model-providers/${selectedProvider.id}`, {
            method: "PATCH",
            body: JSON.stringify(patch),
          })).provider;
        }
        if (form.secret.trim()) {
          saved = (await api(`${apiBase}/model-providers/${selectedProvider.id}/credential/rotate`, {
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
    if (!selectedModel || !selectedModelForm || !writable(selectedModel)) return;
    if (selectedProvider?.isDraft) {
      setError(t("modelSettings.saveProviderBeforeModel"));
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
        ? (await api(`${apiBase}/models`, { method: "POST", body: JSON.stringify(payload) })).model
        : (await api(`${apiBase}/models/${selectedModel.id}`, { method: "PATCH", body: JSON.stringify(payload) })).model;
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
    if (!selectedProvider || !writable(selectedProvider)) return;
    setBusyAction("remove-provider");
    setError("");
    setMessage("");
    try {
      if (selectedProvider.isDraft) {
        setDraftProviders((current) => current.filter((provider) => provider.id !== selectedProvider.id));
        setDraftModels((current) => current.filter((model) => model.providerId !== selectedProvider.id));
      } else {
        await api(`${apiBase}/model-providers/${selectedProvider.id}`, { method: "DELETE" });
        await load();
        await onModelsChanged();
      }
      setSelection({ kind: "empty" });
      setConfirmAction("");
    } catch (removeError) {
      setError(errorText(removeError));
    } finally {
      setBusyAction("");
    }
  }

  async function removeModel() {
    if (!selectedModel || !writable(selectedModel)) return;
    setBusyAction("remove-model");
    setError("");
    setMessage("");
    try {
      if (selectedModel.isDraft) {
        setDraftModels((current) => current.filter((model) => model.id !== selectedModel.id));
      } else {
        await api(`${apiBase}/models/${selectedModel.id}`, { method: "DELETE" });
        await load();
        await onModelsChanged();
      }
      setSelection({ kind: "provider", providerId: selectedModel.providerId });
      setConfirmAction("");
    } catch (removeError) {
      setError(errorText(removeError));
    } finally {
      setBusyAction("");
    }
  }

  async function testModel(model = selectedModel) {
    if (!model || model.isDraft) return;
    setBusyAction("test-model");
    setError("");
    setModelTest(null);
    try {
      const result = await api(`${apiBase}/models/${model.id}/test`, { method: "POST", body: "{}" });
      setModelTest({ ...result, modelId: model.id });
    } catch (testError) {
      setModelTest({ modelId: model.id, ok: false, failureMessage: errorText(testError),
        httpStatus: testError instanceof ApiError ? testError.status : null });
    } finally {
      setBusyAction("");
    }
  }

  return <div className="workspaceModelsLayout">
    <section className="workspaceModelsEditor">
      {pickerOpen ? <section className="workspaceModelsPicker" aria-label={t("interface.addProvider")}>
        <header className="workspaceModelsPickerHeader"><button type="button" aria-label={t("interface.backToModels")} onClick={() => setPickerOpen(false)}><ArrowLeft aria-hidden="true" /></button><strong>{t("interface.addProvider")}</strong></header>
        <div className="workspaceModelsPickerContent"><section><div>{t("interface.custom")}</div><button type="button" className="workspaceModelsCustomCard" onClick={addCustomProvider}><span><strong>{t("interface.custom")}</strong><small>OpenAI / Anthropic</small></span><Plus aria-hidden="true" /></button></section>{PROVIDER_TIERS.map((tier) => {
          const tierTemplates = availableTemplates.filter((template) => template.tier === tier.id);
          return tierTemplates.length ? <section key={tier.id}><div>{tier.label}</div><div className="workspaceModelsPickerGrid">{tierTemplates.map((template) => <button type="button" key={template.id} onClick={() => addTemplateProvider(template)}><span className="workspaceModelsProviderIdentity"><ProviderLogo svg={template.logoSvg} name={template.displayName} /><strong>{template.displayName}</strong></span><span>{t("models.count", { count: template.models.length })}</span></button>)}</div></section> : null;
        })}</div>
      </section> : <>
      <div className="workspaceModelsEditorScroll">
        {selection.kind === "empty" ? <>
          <ModelDefaults key={workspace.id} workspaceId={workspace.id} models={models} />
          <div className="managementToolbar workspaceModelsListToolbar"><button type="button" className="workspaceModelsAddProvider" disabled={Boolean(busyAction)} onClick={openPicker}><Plus aria-hidden="true" />{t("interface.addProvider")}</button></div>
          {busyAction === "load" ? <p role="status">{t("agentChat.loading")}</p> : null}
          {error ? <div className="workspaceModelsEmpty" role="alert">{error}</div> : null}
          {allProviders.length ? <div className="settingsPanel settingsListPanel managementProviderList">{allProviders.map(provider => {
            const name = providerForms[provider.id]?.displayName || provider.displayName;
            return <div key={provider.id}>
              <button type="button" className="managementRow managementRowButton" aria-label={name} onClick={() => showProvider(provider.id)}>
                <span className="managementRowIcon"><ProviderLogo svg={templates.find(template => template.id === provider.templateId)?.logoSvg} name={name} /></span>
                <span className="managementRowCopy"><strong>{name}</strong><small>{provider.apiBase || provider.api}</small></span>
                <span className="managementRowEnd">{t(provider.credentialVersion ? "modelSettings.configured" : "modelSettings.notConfiguredYet")}<ChevronRight aria-hidden="true" /></span>
              </button>
            </div>;
          })}</div> : null}
        </> : <header className="managementDetailHeading">
          <button type="button" className="managementBack" onClick={back} disabled={Boolean(busyAction)} aria-label={t(selection.kind === "model" ? "interface.backToModels" : "interface.modelProviders")}><ArrowLeft aria-hidden="true" /></button>
          <h2>{selection.kind === "model" ? selectedModelForm?.displayName || selectedModelForm?.modelName || t("interface.newModel") : selectedProviderForm?.displayName || selectedProvider?.displayName}</h2>
        </header>}

        {selection.kind === "provider" && selectedProvider && selectedProviderForm ? !writable(selectedProvider) ? <p role="alert">{t("workspaceResources.sharedReadOnly")}</p> : selectedProvider.templateId ? <section className="settingsPanel workspaceModelsPreset">
          <div className="workspaceModelsSectionHeader"><span>{t("interface.apiKey")}</span><small className={selectedProvider.credentialVersion ? "is-configured" : ""}><i aria-hidden="true" />{selectedProvider.credentialVersion ? t("modelSettings.configured") : t("modelSettings.notConfiguredYet")}</small></div>
          <div className="workspaceModelsField">
            <div className="workspaceModelsKeyLine"><div className="workspaceModelsKeyInput"><KeyRound aria-hidden="true" /><input id={providerKeyInputId} name="providerApiKey" aria-label={t("interface.apiKey")} type="password" autoComplete="new-password" spellCheck={false} value={selectedProviderForm.secret} onChange={(event) => updateProviderForm(selectedProvider.id, { secret: event.target.value })} placeholder={selectedProvider.credentialVersion ? t("modelSettings.enterANewKeyToReplaceIt") : t("modelSettings.enterApiKey")} /></div>
              {selectedProviderForm.secret.trim() || busyAction === "save-provider"
                ? <button type="button" className="workspaceModelsInlineSave" disabled={Boolean(busyAction)} onClick={() => void saveProvider()}>{busyAction === "save-provider" ? t("modelSettings.saving") : t("modelSettings.save")}</button>
                : selectedProvider.credentialVersion ? <button type="button" className="workspaceModelsDanger" onClick={() => { setError(""); setConfirmAction("provider"); }} disabled={Boolean(busyAction)}>{t("modelSettings.disconnect")}</button> : null}
            </div>
          </div>
          {!confirmAction && error ? <small className="error" role="alert">{error}</small> : null}
          {message ? <small role="status">{message}</small> : null}
        </section> : <section className="settingsPanel">
          <div className="workspaceModelsSectionHeader"><span>{t("interface.customProvider")}</span><button type="button" className="workspaceModelsDanger" onClick={() => { setError(""); setConfirmAction("provider"); }} disabled={Boolean(busyAction)}>{t("globalPluginSettings.remove")}</button></div>
          <label className="workspaceModelsField"><span>{t("modelSettings.providerName")}</span><input value={selectedProviderForm.nameInput} onChange={(event) => updateProviderForm(selectedProvider.id, { nameInput: event.target.value })} /></label>
          <button type="button" className="workspaceModelsRename" disabled={Boolean(busyAction)} onClick={() => void renameProvider()}>{busyAction === "rename" ? t("modelSettings.saving") : t("appRoute.rename")}</button>
          <label className="workspaceModelsField"><span>{t("interface.baseUrl")}</span><input type="url" value={selectedProviderForm.apiBase} onChange={(event) => updateProviderForm(selectedProvider.id, { apiBase: event.target.value })} placeholder="https://api.example.com/v1" /></label>
          <label className="workspaceModelsField"><span>{t("modelSettings.apiProtocol")}</span><select value={selectedProviderForm.api} onChange={(event) => updateProviderForm(selectedProvider.id, { api: event.target.value })}>{API_OPTIONS.map((api) => <option key={api} value={api}>{api}</option>)}</select></label>
          <section className="workspaceModelsKeySection">
            <div className="workspaceModelsSectionHeader"><span>{t("interface.apiKey")}</span><small className={selectedProvider.credentialVersion ? "is-configured" : ""}>{selectedProvider.credentialVersion ? t("modelSettings.configured") : t("modelSettings.notConfiguredYet")}</small></div>
            <div className="workspaceModelsKeyInput"><KeyRound aria-hidden="true" /><input id={providerKeyInputId} name="providerApiKey" aria-label={t("interface.apiKey")} type="password" autoComplete="new-password" spellCheck={false} value={selectedProviderForm.secret} onChange={(event) => updateProviderForm(selectedProvider.id, { secret: event.target.value })} placeholder={selectedProvider.credentialVersion ? t("modelSettings.enterANewKeyToReplaceIt") : t("modelSettings.enterApiKey")} /></div>
          </section>
        </section> : null}

        {selection.kind === "provider" && selectedProvider ? <section className="settingsPanel managementModelList" aria-label={t("appRoute.models")}>
          <div className="managementToolbar"><h3>{t("appRoute.models")} <small>{providerModels.length}</small></h3>
            {writable(selectedProvider) && !selectedProvider.templateId ? <button type="button" className="workspaceModelsAddProvider" disabled={Boolean(busyAction) || selectedProvider.isDraft} onClick={() => addModel(selectedProvider.id)}><Plus aria-hidden="true" />{t("models.addModel")}</button> : null}
          </div>
          {providerModels.map(model => {
            const form = modelForms[model.id];
            const name = form?.displayName || form?.modelName || t("interface.newModel");
            return <div className="managementModelItem" key={model.id}>
              <div className="managementModelRow">
                <button type="button" className="managementRow managementRowButton" disabled={Boolean(busyAction)} onClick={() => setSelection({ kind: "model", providerId: selectedProviderId, modelId: model.id })}>
                  <span className="managementRowCopy"><strong>{name}</strong>{form?.modelName && form.modelName !== name ? <small>{form.modelName}</small> : null}</span>
                  <span className="managementRowEnd"><ChevronRight aria-hidden="true" /></span>
                </button>
                {writable(model) ? <button className="workspaceModelsTest" type="button" disabled={Boolean(busyAction) || model.isDraft} onClick={() => void testModel(model)}>{t("interface.test")}</button> : null}
              </div>
              {modelTest?.modelId === model.id ? <div className={modelTest.ok ? "workspaceModelsTestResult is-ok" : "workspaceModelsTestResult is-error"} role="status">{testSummary(modelTest, t)}</div> : null}
            </div>;
          })}
        </section> : null}

        {selection.kind === "model" && selectedModel && selectedModelForm ? <fieldset disabled={!writable(selectedModel)} className="settingsPanel workspaceModelsForm">
          {!writable(selectedModel) ? <p>{t("workspaceResources.sharedReadOnly")}</p> : null}
          <div className="workspaceModelsSectionHeader"><span>{t("interface.model")}</span><span><button type="button" className="workspaceModelsTest" disabled={selectedModel.isDraft || Boolean(busyAction)} onClick={() => void testModel()}>{busyAction === "test-model" ? t("interface.testing") : t("interface.test")}</button><button type="button" className="workspaceModelsDanger" disabled={Boolean(busyAction)} onClick={() => { setError(""); setConfirmAction("model"); }}>{t("interface.remove")}</button></span></div>
          {selectedTest ? <div className={selectedTest.ok ? "workspaceModelsTestResult is-ok" : "workspaceModelsTestResult is-error"}>{testSummary(selectedTest, t)}</div> : null}
          <div className="workspaceModelsFormGrid">
            <label className="workspaceModelsField"><span>ID *</span><input value={selectedModelForm.modelName} onChange={(event) => updateModelForm(selectedModel.id, { modelName: event.target.value })} placeholder="model-id" /></label>
            <label className="workspaceModelsField"><span>{t("interface.name")}</span><input value={selectedModelForm.displayName} onChange={(event) => updateModelForm(selectedModel.id, { displayName: event.target.value })} placeholder={t("interface.displayName")} /></label>
          </div>
          <label className="workspaceModelsField"><span>{t("interface.apiOverride")}</span><select value={selectedModelForm.apiOverride} onChange={(event) => updateModelForm(selectedModel.id, { apiOverride: event.target.value })}><option value="">{t("interface.inheritNone")}</option>{API_OPTIONS.map((api) => <option key={api} value={api}>{api}</option>)}</select></label>
          <div className="workspaceModelsFormGrid"><label className="workspaceModelsField"><span>{t("interface.contextWindowTokens")}</span><input inputMode="numeric" value={selectedModelForm.contextTokens} onChange={(event) => updateModelForm(selectedModel.id, { contextTokens: event.target.value })} /></label><label className="workspaceModelsField"><span>{t("interface.maxOutputTokens")}</span><input inputMode="numeric" value={selectedModelForm.maxOutputTokens} onChange={(event) => updateModelForm(selectedModel.id, { maxOutputTokens: event.target.value })} /></label></div>
          <div className="workspaceModelsFormGrid"><label className="workspaceModelsField"><span>{t("interface.defaultThinkingEffort")}</span><input value={selectedModelForm.thinkingMode} onChange={(event) => updateModelForm(selectedModel.id, { thinkingMode: event.target.value })} placeholder={t("interface.highOptional")} /></label><label className="workspaceModelsField"><span>{t("interface.supportedEfforts")}</span><input value={selectedModelForm.thinkingModes} onChange={(event) => updateModelForm(selectedModel.id, { thinkingModes: event.target.value })} placeholder="low, medium, high" /></label></div>
        </fieldset> : null}
      </div>
      {selection.kind !== "empty" && !(selectedProvider?.templateId && selection.kind === "provider") ? <footer className="workspaceModelsActions">
        {!confirmAction && error ? <small className="error" role="alert">{error}</small> : message ? <small role="status">{message}</small> : null}
        <button type="button" onClick={back} disabled={Boolean(busyAction)}>{t("agentRunRow.cancel")}</button><button type="button" className="is-primary" disabled={Boolean(busyAction) || !writable(selection.kind === "provider" ? selectedProvider : selectedModel)} onClick={() => void (selection.kind === "provider" ? saveProvider() : saveModel())}>{busyAction.startsWith("save") ? t("modelSettings.saving") : t("modelSettings.save")}</button></footer> : null}
      </>}
    </section>
    <ConfirmDialog
      open={Boolean(confirmAction)}
      title={confirmAction === "provider" ? (selectedProvider?.templateId ? t("modelSettings.disconnectProvider") : t("modelSettings.removeProvider")) : t("modelSettings.removeModel")}
      message={error || (confirmAction === "provider"
        ? t("modelSettings.valueAndItsModelsWillNoLongerBeAvailable", { value1: selectedProviderForm?.displayName || selectedProvider?.displayName || t("modelSettings.provider") })
        : t("modelSettings.removeValue", { value1: selectedModelForm?.modelName || t("appRoute.models") }))}
      confirmLabel={selectedProvider?.templateId ? t("modelSettings.disconnect") : t("globalPluginSettings.remove")}
      cancelLabel={t("agentRunRow.cancel")}
      busy={busyAction === "remove-provider" || busyAction === "remove-model"}
      onCancel={() => setConfirmAction("")}
      onConfirm={() => void (confirmAction === "provider" ? removeProvider() : removeModel())}
    />
  </div>;
}
