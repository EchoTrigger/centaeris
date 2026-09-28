import { useCallback, useEffect, useRef, useState } from "react";
import { Eye, EyeOff, KeyRound, Plus, Search, ChevronRight, MoreHorizontal } from "lucide-react";
import {
  getAgentRuntimeConfig,
  resetAgentRuntimeConfig,
  setAgentRuntimeConfig,
  testAgentRuntimeModel,
  type AgentRuntimeConfig,
  type AgentRuntimeModelTestResponse,
  type CustomModelProviderInput,
  type ModelWireApi,
} from "../lib/chatBridge";
import type { ConfirmAction } from "./ConfirmDialog";
import { ResourceDetailDialog } from "./ResourceDetailDialog";
import { ProviderLogo } from "./ProviderLogo";

type ModelsDialogProps = {
  onClose: () => void;
  onConfigured?: (configured: boolean) => void;
  confirmAction: ConfirmAction;
};

type Selection =
  | { kind: "empty" }
  | { kind: "provider"; providerId: string }
  | { kind: "model"; providerId: string; modelKey: number | string };

type CustomModelDraft = {
  key: number;
  model: string;
  displayName: string;
  contextTokens: string;
  maxOutputTokens: string;
  apiOverride?: ModelWireApi;
  supportsVision: boolean;
};

type CustomProviderDraft = {
  providerId: string;
  name: string;
  baseUrl: string;
  api: ModelWireApi;
  models: CustomModelDraft[];
};

type ModelTestState = AgentRuntimeModelTestResponse & {
  providerId: string;
  model: string;
};

const API_OPTIONS: ModelWireApi[] = [
  "openai-completions",
  "openai-responses",
  "anthropic-messages",
];

let nextCustomModelKey = 1;

const newCustomModelDraft = (): CustomModelDraft => ({
  key: nextCustomModelKey++,
  model: "",
  displayName: "",
  contextTokens: "128k",
  maxOutputTokens: "32k",
  supportsVision: false,
});

const errorText = (error: unknown): string =>
  error instanceof Error ? error.message : String(error || "model_configuration_failed");

const customProviderId = (): string => `custom.${crypto.randomUUID().replaceAll("-", "")}`;

const customDraftsFromConfig = (config: AgentRuntimeConfig): CustomProviderDraft[] =>
  (config.customModelProviders ?? []).map((provider) => ({
    providerId: provider.providerId,
    name: provider.name,
    baseUrl: provider.baseUrl,
    api: provider.api,
    models: provider.models.map((model) => ({
        key: nextCustomModelKey++,
        model: model.model,
        displayName: model.displayName ?? "",
        contextTokens: model.contextTokens,
        maxOutputTokens: model.maxOutputTokens,
        apiOverride: model.apiOverride,
        supportsVision: model.supportsVision,
      })),
  }));

export const buildCustomProvidersInput = (
  providers: CustomProviderDraft[],
): CustomModelProviderInput[] => providers.map((provider) => ({
  providerId: provider.providerId,
  name: provider.name,
  baseUrl: provider.baseUrl,
  api: provider.api,
  models: provider.models.map((model) => ({
    model: model.model,
    displayName: model.displayName || undefined,
    contextTokens: model.contextTokens,
    maxOutputTokens: model.maxOutputTokens,
    apiOverride: model.apiOverride,
    supportsVision: model.supportsVision,
  })),
}));

const testSucceeded = (result: ModelTestState): boolean =>
  result.httpStatus !== null
  && result.httpStatus !== undefined
  && result.httpStatus >= 200
  && result.httpStatus < 300
  && !result.errorKeyword;

const testSummary = (result: ModelTestState): string => {
  const status = result.httpStatus ? `HTTP ${result.httpStatus}` : null;
  if (testSucceeded(result)) {
    return ["Connected", `${result.latencyMs}ms`, status, result.outputPreview?.trim() || "OK"]
      .filter(Boolean)
      .join(" · ");
  }
  return ["Failed", `${result.latencyMs}ms`, status, result.errorKeyword?.trim() || "unknown_error"]
    .filter(Boolean)
    .join(" · ");
};

export function ModelsDialog({ onConfigured, confirmAction }: ModelsDialogProps) {
  const [editing, setEditing] = useState(false);
  const [modelQuery, setModelQuery] = useState("");
  const [config, setConfig] = useState<AgentRuntimeConfig | null>(null);
  const [customProviders, setCustomProviders] = useState<CustomProviderDraft[]>([]);
  const [selection, setSelection] = useState<Selection>({ kind: "empty" });
  const [apiKeys, setApiKeys] = useState<Record<string, string>>({});
  const [revealedProviderId, setRevealedProviderId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [savingSettings, setSavingSettings] = useState(false);
  const [credentialProviderId, setCredentialProviderId] = useState("");
  const [testingModelId, setTestingModelId] = useState("");
  const [message, setMessage] = useState("");
  const [modelTest, setModelTest] = useState<ModelTestState | null>(null);
  const [canResetUnsupportedConfig, setCanResetUnsupportedConfig] = useState(false);
  const [providerPickerOpen, setProviderPickerOpen] = useState(false);
  const [providerQuery, setProviderQuery] = useState("");
  const loadSequence = useRef(0);
  const select = useCallback((next: Selection) => {
    setSelection(next);
    setModelTest(null);
  }, []);

  const acceptConfig = useCallback((next: AgentRuntimeConfig) => {
    setConfig(next);
    onConfigured?.((next.selectableModels ?? []).length > 0);
  }, [onConfigured]);

  const load = useCallback(async () => {
    const sequence = ++loadSequence.current;
    setLoading(true);
    setMessage("");
    try {
      const next = await getAgentRuntimeConfig();
      if (sequence !== loadSequence.current) return;
      acceptConfig(next);
      setCustomProviders(customDraftsFromConfig(next));
      setApiKeys({});
      setModelTest(null);
      setCanResetUnsupportedConfig(false);
    } catch (error) {
      if (sequence !== loadSequence.current) return;
      const nextMessage = errorText(error);
      setConfig(null);
      setMessage(nextMessage);
      setCanResetUnsupportedConfig(
        nextMessage.includes("runtime_config_unsupported")
        || nextMessage.includes("runtime_secret_unsupported"),
      );
    } finally {
      if (sequence === loadSequence.current) setLoading(false);
    }
  }, [acceptConfig]);

  const resetUnsupportedConfig = async () => {
    if (!canResetUnsupportedConfig || loading || savingSettings) return;
    setMessage("");
    try {
      const confirmed = await confirmAction({
        title: "Reset model settings?",
        message: "This removes all API keys saved by Centaeris. Environment credentials stay unchanged.",
      });
      if (!confirmed) return;
      setSavingSettings(true);
      const response = await resetAgentRuntimeConfig();
      acceptConfig(response.config);
      setCustomProviders([]);
      setApiKeys({});
      select({ kind: "empty" });
      setCanResetUnsupportedConfig(false);
      setMessage("Runtime configuration reset");
    } catch (error) {
      setMessage(errorText(error));
    } finally {
      setSavingSettings(false);
    }
  };

  useEffect(() => {
    void load();
  }, [load]);

  const selectedProviderId = selection.kind === "empty" ? null : selection.providerId;
  const selectedCustomProvider = customProviders.find((provider) => provider.providerId === selectedProviderId);
  const selectedCustomModel = selection.kind === "model" && selectedCustomProvider
    ? selectedCustomProvider.models.find((model) => model.key === selection.modelKey)
    : undefined;
  const selectedModelId = selection.kind === "model"
    ? (typeof selection.modelKey === "string" ? selection.modelKey : selectedCustomModel?.model.trim())
    : undefined;
  const selectedProvider = config?.modelProviders.find((provider) => provider.providerId === selectedProviderId);
  const selectedCatalogModel = selection.kind === "model"
    ? config?.modelProviders.flatMap((provider) => provider.models).find((model) => (
      model.providerId === selection.providerId && model.model === selectedModelId
    ))
    : undefined;
  const updateCustomProvider = (patch: Partial<CustomProviderDraft>) => {
    if (!selectedCustomProvider) return;
    setCustomProviders((providers) => providers.map((provider) => (
      provider.providerId === selectedCustomProvider.providerId ? { ...provider, ...patch } : provider
    )));
  };

  const updateCustomModel = (key: number, patch: Partial<CustomModelDraft>) => {
    if (!selectedCustomProvider) return;
    setCustomProviders((providers) => providers.map((provider) => provider.providerId !== selectedCustomProvider.providerId
      ? provider
      : { ...provider, models: provider.models.map((model) => model.key === key ? { ...model, ...patch } : model) }));
    setModelTest(null);
  };

  const cancelEdit = () => {
    if (savingSettings || credentialProviderId) return;
    if (config) setCustomProviders(customDraftsFromConfig(config));
    setApiKeys({}); setRevealedProviderId(null); setEditing(false); setMessage("");
    select(selectedProviderId && config?.modelProviders.some(p => p.providerId === selectedProviderId)
      ? {kind:"provider",providerId:selectedProviderId} : {kind:"empty"});
  };
  const saveAll = async () => {
    if (loading || savingSettings || !selectedProviderId) return;
    setSavingSettings(true); setMessage("");
    let settingsSaved = false;
    try {
      if (selectedCustomProvider) {
        const next = await setAgentRuntimeConfig({customModelProviders: buildCustomProvidersInput(customProviders)});
        acceptConfig(next); settingsSaved = true;
      }
      const key = apiKeys[selectedProviderId]?.trim();
      if (key) {
        const next = await setAgentRuntimeConfig({modelProviderId:selectedProviderId,modelApiKey:key});
        acceptConfig(next);
        setApiKeys({});
      }
      setEditing(false); setRevealedProviderId(null);
      select({kind:"provider",providerId:selectedProviderId});
      setMessage("Saved");
    } catch (error) {
      setMessage(`${settingsSaved ? "Connection settings saved, but the API key was not saved. Retry saving the key. " : ""}${errorText(error)}`);
    } finally { setSavingSettings(false); }
  };

  const clearCredential = async (providerId: string) => {
    if (credentialProviderId) return;
    setCredentialProviderId(providerId);
    setMessage("");
    try {
      const next = await setAgentRuntimeConfig({
        modelProviderId: providerId,
        clearModelApiKey: true,
      });
      acceptConfig(next);
      select({ kind: "provider", providerId });
      setMessage("Credential removed");
    } catch (error) {
      setMessage(errorText(error));
    } finally {
      setCredentialProviderId("");
    }
  };

  const addCustomProvider = () => {
    const provider: CustomProviderDraft = {
      providerId: customProviderId(),
      name: "New provider",
      baseUrl: "",
      api: "openai-completions",
      models: [],
    };
    setCustomProviders((providers) => [...providers, provider]);
    setEditing(true);
    select({ kind: "provider", providerId: provider.providerId });
    setProviderPickerOpen(false);
    setMessage("");
  };

  const addModel = (providerId: string) => {
    const provider = customProviders.find((item) => item.providerId === providerId);
    if (!provider) return;
    const model = newCustomModelDraft();
    setCustomProviders((providers) => providers.map((item) => item.providerId === providerId
      ? { ...item, models: [...item.models, model] }
      : item));
    setEditing(true);
    select({ kind: "model", providerId, modelKey: model.key });
  };

  const removeSelectedModel = async () => {
    if (!selectedCustomProvider || !selectedCustomModel) return;
    setMessage("");
    try {
      const confirmed = await confirmAction({
        title: "Remove this model?",
        message: `“${selectedCustomModel.model || "New model"}” will be removed from this provider.`,
      });
      if (!confirmed) return;
      const providers = customProviders.map(provider => provider.providerId !== selectedCustomProvider.providerId ? provider : { ...provider, models: provider.models.filter(model => model.key !== selectedCustomModel.key) });
      const next = await setAgentRuntimeConfig({customModelProviders:buildCustomProvidersInput(providers)});
      acceptConfig(next); setCustomProviders(customDraftsFromConfig(next)); setEditing(false);
      select({ kind: "provider", providerId: selectedCustomProvider.providerId });
    } catch (error) {
      setMessage(errorText(error));
    }
  };

  const removeSelectedProvider = async () => {
    if (!selectedCustomProvider) return;
    setMessage("");
    try {
      const confirmed = await confirmAction({
        title: "Remove this provider?",
        message: `“${selectedCustomProvider.name}” and its models will be removed.`,
      });
      if (!confirmed) return;
      const next = await setAgentRuntimeConfig({customModelProviders:buildCustomProvidersInput(customProviders.filter(provider => provider.providerId !== selectedCustomProvider.providerId))});
      acceptConfig(next); setCustomProviders(customDraftsFromConfig(next)); setEditing(false);
      select({ kind: "empty" });
    } catch (error) {
      setMessage(errorText(error));
    }
  };

  const runTest = async (modelId = selectedModelId) => {
    const catalogModel = selectedProvider?.models.find(model => model.model === modelId);
    if (!catalogModel || !selectedProviderId || catalogModel.diagnostic || testingModelId) return;
    const testId = `${selectedProviderId}\0${catalogModel.model}`;
    setTestingModelId(testId);
    setMessage("");
    try {
      const result = await testAgentRuntimeModel({ providerId: selectedProviderId, model: catalogModel.model });
      setModelTest({ ...result, providerId: selectedProviderId, model: catalogModel.model });
    } catch (error) {
      setModelTest({
        providerId: selectedProviderId,
        model: catalogModel.model,
        httpStatus: null,
        latencyMs: 0,
        errorKeyword: errorText(error),
      });
    } finally {
      setTestingModelId("");
    }
  };

  const providerStored = selectedProvider?.credentialSource === "stored";
  const credentialLabel = (provider: typeof selectedProvider) => provider?.credentialSource === "environment" ? "Using environment credentials" : provider?.credentialSource === "stored" ? "API key configured" : provider?.configured ? "Configured" : "No credential configured";
  const selectedTestId = selectedProviderId && selectedCatalogModel
    ? `${selectedProviderId}\0${selectedCatalogModel.model}`
    : "";
  const testForSelectedModel = modelTest
    && selection.kind === "model"
    && modelTest.providerId === selection.providerId
    && modelTest.model === selectedCatalogModel?.model
    ? modelTest
    : null;
  const normalizedProviderQuery = providerQuery.trim().toLocaleLowerCase();
  const providerMatches = (...values: string[]): boolean => !normalizedProviderQuery
    || values.some((value) => value.toLocaleLowerCase().includes(normalizedProviderQuery));
  const customCandidateVisible = providerMatches(
    "OpenAI Anthropic compatible",
    "Custom endpoint HTTP HTTPS",
  );
  const visibleApiProviders = (config?.modelProviders ?? []).filter((provider) => provider.builtIn &&
    providerMatches(provider.name, "API key HTTPS"));
  const providerPickerEmpty = !customCandidateVisible
    && visibleApiProviders.length === 0;
  const busy = savingSettings || Boolean(credentialProviderId);
  const openProvider = (providerId: string) => { setMessage(""); setModelQuery(""); setEditing(false); select({kind:"provider",providerId}); };
  const listedProviders = (config?.modelProviders ?? []).filter(p => p.configured || !p.builtIn);
  const modelRows = selectedCustomProvider ? selectedCustomProvider.models.map(m => ({key:m.key, id:m.model, name:m.displayName || m.model})) : (selectedProvider?.models ?? []).map(m => ({key:m.model,id:m.model,name:m.model}));
  const saveButtons = <footer className="modelEditActions"><button type="button" onClick={cancelEdit} disabled={busy}>Cancel</button><button type="button" onClick={() => void saveAll()} disabled={busy || !config || (!selectedCustomProvider && !apiKeys[selectedProviderId ?? ""]?.trim())}>{busy ? "Saving…" : "Save"}</button></footer>;
  return <div className="modelServices">
    {selection.kind === "empty" ? <>
      <header className="modelServicesHeading"><h1>Model services</h1><button type="button" disabled={!config || loading} onClick={() => {setProviderQuery("");setProviderPickerOpen(true);}}><Plus size={16}/> Add service</button></header>
      <div className="modelServiceList">{listedProviders.map(provider => <button type="button" className="modelServiceRow" key={provider.providerId} aria-label={`Open ${provider.name}`} onClick={() => openProvider(provider.providerId)}>
        <ProviderLogo svg={provider.logoSvg} name={provider.name}/><span><strong>{provider.name}</strong><small>{credentialLabel(provider)}</small></span><ChevronRight size={16}/>
      </button>)}</div>
      {!loading && config && !listedProviders.length ? <p className="pageDescription">Add a service to configure your models.</p> : null}
    </> : <>
      <nav className="extensionBreadcrumb" aria-label="Model service navigation"><button type="button" disabled={editing || busy} onClick={() => {select({kind:"empty"});setMessage("");}}>Model services</button><ChevronRight size={14}/><span>{selectedCustomProvider?.name ?? selectedProvider?.name}</span></nav>
      <header className="modelServicesHeading"><h1>{selectedCustomProvider?.name ?? selectedProvider?.name}</h1>
        {!editing ? <details className="modelServiceMenu"><summary aria-label="Service actions"><MoreHorizontal size={18}/></summary><div>{providerStored ? <button type="button" disabled={busy} onClick={() => void clearCredential(selectedProviderId!)}>Remove credential</button> : null}{selectedCustomProvider ? <button type="button" disabled={busy} onClick={() => void removeSelectedProvider()}>Remove service</button> : null}</div></details> : null}
      </header>
      <section className="modelConnection"><header><h2>Connection</h2>{!editing ? <button type="button" onClick={() => {setMessage("");setEditing(true);}}>Edit connection</button> : null}</header>
        {editing && selection.kind === "provider" ? <>
          <div className="modelConnectionForm">
            {selectedCustomProvider ? <><label>Provider name<input aria-label="Provider name" value={selectedCustomProvider.name} onChange={e=>updateCustomProvider({name:e.target.value})}/></label><label>Base URL<input value={selectedCustomProvider.baseUrl} onChange={e=>updateCustomProvider({baseUrl:e.target.value})} placeholder="https://api.example.com/v1"/></label><label>API protocol<select value={selectedCustomProvider.api} onChange={e=>updateCustomProvider({api:e.target.value as ModelWireApi})}>{API_OPTIONS.map(api=><option key={api}>{api}</option>)}</select></label></> : null}
            <label>API key<div className="modelsKeyInput"><KeyRound size={16}/><input aria-label="API key" autoComplete="off" type={revealedProviderId === selectedProviderId ? "text" : "password"} value={apiKeys[selectedProviderId!] ?? ""} onChange={e=>setApiKeys(keys=>({...keys,[selectedProviderId!]:e.target.value}))} placeholder={selectedProvider?.configured ? "Leave blank to keep current credential" : "Enter API key"}/><button type="button" aria-label="Toggle API key visibility" onClick={()=>setRevealedProviderId(revealedProviderId ? null : selectedProviderId)}>{revealedProviderId ? <EyeOff size={16}/> : <Eye size={16}/>}</button></div></label>
          </div>{saveButtons}
        </> : <dl className="modelConnectionFacts"><div><dt>Credential</dt><dd>{credentialLabel(selectedProvider)}</dd></div>{selectedCustomProvider ? <><div><dt>Base URL</dt><dd>{selectedCustomProvider.baseUrl}</dd></div><div><dt>API protocol</dt><dd>{selectedCustomProvider.api}</dd></div></> : null}</dl>}
      </section>
      {!editing || selection.kind === "model" ? <section className="modelServiceModels"><header><h2>Models</h2><label className="extensionSearch"><Search size={16}/><input aria-label="Search models" placeholder="Search models" value={modelQuery} onChange={e=>setModelQuery(e.target.value)}/></label>{selectedCustomProvider ? <button type="button" onClick={()=>addModel(selectedProviderId!)}><Plus size={16}/> Add model</button> : null}</header>
        {modelRows.filter(m=>`${m.id} ${m.name}`.toLowerCase().includes(modelQuery.toLowerCase())).map(model=><div className="modelServiceModelRow" key={model.key}><button type="button" onClick={()=>{setMessage("");setEditing(true);select({kind:"model",providerId:selectedProviderId!,modelKey:model.key});}}>{model.name || "New model"}</button><small>{model.name !== model.id ? model.id : ""}</small><button type="button" aria-label={`Test ${model.id}`} disabled={!!testingModelId || !selectedProvider?.models.some(m => m.model === model.id && !m.diagnostic)} onClick={()=>void runTest(model.id)}>{testingModelId === `${selectedProviderId}\0${model.id}` ? "Testing…" : "Test"}</button></div>)}
        {modelTest?.providerId === selectedProviderId && selection.kind === "provider" ? <p className="modelsTestSummary">{modelTest.model}: {testSummary(modelTest)}</p> : null}
        {!modelRows.length ? <p className="pageDescription">No models added.</p> : null}
      </section> : null}
    </>}
    {message || loading ? <p className="modelServiceMessage" role="status">{message || "Loading…"}</p> : null}
    {canResetUnsupportedConfig ? <button type="button" disabled={loading || busy} onClick={()=>void resetUnsupportedConfig()}>Reset configuration…</button> : null}
    {selection.kind === "model" ? <ResourceDetailDialog title={selectedModelId || "New model"} onClose={cancelEdit}>
          {selectedCustomModel ? <section className="modelsSection">
            <div className="modelsSectionHeading"><span>MODEL</span><span className="modelsModelActions"><button type="button" className={testForSelectedModel && testSucceeded(testForSelectedModel) ? "modelsTestButton is-success" : "modelsTestButton"} disabled={!selectedCatalogModel || Boolean(selectedCatalogModel.diagnostic) || Boolean(testingModelId)} onClick={() => void runTest()}>{testingModelId === selectedTestId ? "Testing…" : testForSelectedModel ? (testSucceeded(testForSelectedModel) ? "OK" : testForSelectedModel.httpStatus ? `HTTP ${testForSelectedModel.httpStatus}` : "Failed") : "Test"}</button>{selectedCustomModel ? <button type="button" className="modelsDangerButton" onClick={() => void removeSelectedModel()}>Remove</button> : null}</span></div>
            {selectedCatalogModel?.diagnostic ? <div className="modelsDiagnostic">{selectedCatalogModel.diagnostic}</div> : null}
            {testForSelectedModel ? <div className={testSucceeded(testForSelectedModel) ? "modelsTestSummary is-success" : "modelsTestSummary"}>{testSummary(testForSelectedModel)}</div> : null}
            <div className="modelsModelForm">
              <label><span>ID *</span><input value={selectedCustomModel.model} onChange={(event) => updateCustomModel(selectedCustomModel.key, { model: event.target.value })} placeholder="model-id" /></label>
              <label><span>Name</span><input value={selectedCustomModel.displayName} onChange={(event) => updateCustomModel(selectedCustomModel.key, { displayName: event.target.value })} placeholder="Optional home label" /></label>
              <label><span>API override</span><select value={selectedCustomModel.apiOverride ?? ""} onChange={(event) => updateCustomModel(selectedCustomModel.key, { apiOverride: event.target.value ? event.target.value as ModelWireApi : undefined })}><option value="">Inherit provider</option>{API_OPTIONS.map((api) => <option key={api} value={api}>{api}</option>)}</select></label>
              <label><span>Context window</span><input value={selectedCustomModel.contextTokens} onChange={(event) => updateCustomModel(selectedCustomModel.key, { contextTokens: event.target.value })} /></label>
              <label><span>Max output</span><input value={selectedCustomModel.maxOutputTokens} onChange={(event) => updateCustomModel(selectedCustomModel.key, { maxOutputTokens: event.target.value })} /></label>
              <label><span>Image input</span><input type="checkbox" checked={selectedCustomModel.supportsVision} onChange={(event) => updateCustomModel(selectedCustomModel.key, { supportsVision: event.target.checked })} /></label>
            </div>
          </section> : null}
      {!selectedCustomModel ? <section><h2>{selectedModelId}</h2>{selectedCatalogModel?.diagnostic ? <p>{selectedCatalogModel.diagnostic}</p> : null}<button type="button" disabled={!selectedCatalogModel || !!selectedCatalogModel.diagnostic || !!testingModelId} onClick={()=>void runTest()}>{testingModelId ? "Testing…" : "Test"}</button>{testForSelectedModel ? <p className="modelsTestSummary">{testSummary(testForSelectedModel)}</p> : null}</section> : saveButtons}
      {message ? <p role="status">{message}</p> : null}
    </ResourceDetailDialog> : null}
    {providerPickerOpen ? <ResourceDetailDialog title="Add service" onClose={()=>setProviderPickerOpen(false)}>
      <h2>Add service</h2><label className="extensionSearch"><Search size={16}/><input aria-label="Search providers" placeholder="Search services" value={providerQuery} onChange={e=>setProviderQuery(e.target.value)}/></label>
      <div className="modelServicePicker">
        {customCandidateVisible ? <button type="button" onClick={addCustomProvider}><Plus size={16}/><span>Custom service<small>OpenAI / Anthropic compatible</small></span></button> : null}
        {visibleApiProviders.map(provider=><button type="button" key={provider.providerId} onClick={()=>{openProvider(provider.providerId);setEditing(true);setProviderPickerOpen(false);}}><ProviderLogo svg={provider.logoSvg} name={provider.name}/><span>{provider.name}</span></button>)}
        {providerPickerEmpty ? <p>No services found.</p> : null}
      </div>
    </ResourceDetailDialog> : null}
  </div>;
}

export default ModelsDialog;
