import { useCallback, useEffect, useRef, useState } from "react";
import { Eye, EyeOff, KeyRound, Plus, Search, X } from "lucide-react";
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

const OAUTH_SUBSCRIPTIONS = [
  { name: "ChatGPT / Codex", models: "GPT-5.6 Sol · Terra · Luna" },
  { name: "Claude", models: "Opus 5 · Sonnet 5 · Fable 5" },
] as const;

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

export function ModelsDialog({ onClose, onConfigured, confirmAction }: ModelsDialogProps) {
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
      setSelection({ kind: "empty" });
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

  useEffect(() => {
    if (!providerPickerOpen) return undefined;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setProviderPickerOpen(false);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [providerPickerOpen]);

  const selectedProviderId = selection.kind === "empty" ? null : selection.providerId;
  const selectedCustomProvider = customProviders.find((provider) => provider.providerId === selectedProviderId);
  const selectedCustomModel = selection.kind === "model" && selectedCustomProvider
    ? selectedCustomProvider.models.find((model) => model.key === selection.modelKey)
    : undefined;
  const selectedBuiltInProvider = config?.modelProviders.find((provider) => (
    provider.builtIn && provider.providerId === selectedProviderId
  ));
  const selectedModelId = selection.kind === "model"
    ? (typeof selection.modelKey === "string" ? selection.modelKey : selectedCustomModel?.model.trim())
    : undefined;
  const selectedProvider = config?.modelProviders.find((provider) => provider.providerId === selectedProviderId);
  const selectedCatalogModel = selection.kind === "model"
    ? config?.modelProviders.flatMap((provider) => provider.models).find((model) => (
      model.providerId === selection.providerId && model.model === selectedModelId
    ))
    : undefined;
  const visibleBuiltIns = (config?.modelProviders ?? []).filter((provider) => (
    provider.builtIn && (provider.configured
    || provider.providerId === selectedProviderId
    )
  ));

  useEffect(() => setModelTest(null), [selectedProviderId, selectedModelId]);

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

  const saveAll = async () => {
    if (loading || savingSettings) return;
    setSavingSettings(true);
    setMessage("");
    try {
      const next = await setAgentRuntimeConfig({
        customModelProviders: buildCustomProvidersInput(customProviders),
      });
      acceptConfig(next);
      setMessage("Saved");
    } catch (error) {
      setMessage(errorText(error));
    } finally {
      setSavingSettings(false);
    }
  };

  const saveCredential = async (providerId: string) => {
    const apiKey = apiKeys[providerId]?.trim() ?? "";
    if (!apiKey || credentialProviderId) return;
    setCredentialProviderId(providerId);
    setMessage("");
    try {
      const next = await setAgentRuntimeConfig({
        modelProviderId: providerId,
        modelApiKey: apiKey,
      });
      acceptConfig(next);
      setApiKeys((keys) => ({ ...keys, [providerId]: "" }));
      setMessage("Credential saved");
    } catch (error) {
      setMessage(errorText(error));
    } finally {
      setCredentialProviderId("");
    }
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
      setSelection({ kind: "provider", providerId });
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
    setSelection({ kind: "provider", providerId: provider.providerId });
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
    setSelection({ kind: "model", providerId, modelKey: model.key });
    setModelTest(null);
  };

  const removeSelectedModel = async () => {
    if (!selectedCustomProvider || !selectedCustomModel) return;
    setMessage("");
    try {
      const confirmed = await confirmAction({
        title: "Remove this model?",
        message: `“${selectedCustomModel.model || "New model"}” will be removed when you save the provider settings.`,
      });
      if (!confirmed) return;
      setCustomProviders((providers) => providers.map((provider) => provider.providerId !== selectedCustomProvider.providerId
        ? provider
        : { ...provider, models: provider.models.filter((model) => model.key !== selectedCustomModel.key) }));
      setSelection({ kind: "provider", providerId: selectedCustomProvider.providerId });
      setModelTest(null);
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
        message: `“${selectedCustomProvider.name}” and its model drafts will be removed when you save the model settings.`,
      });
      if (!confirmed) return;
      setCustomProviders((providers) => providers.filter((provider) => provider.providerId !== selectedCustomProvider.providerId));
      setSelection({ kind: "empty" });
      setModelTest(null);
    } catch (error) {
      setMessage(errorText(error));
    }
  };

  const runTest = async () => {
    if (!selectedCatalogModel || !selectedProviderId || selectedCatalogModel.diagnostic || testingModelId) return;
    const testId = `${selectedProviderId}\0${selectedCatalogModel.model}`;
    setTestingModelId(testId);
    setMessage("");
    try {
      const result = await testAgentRuntimeModel({ providerId: selectedProviderId, model: selectedCatalogModel.model });
      setModelTest({ ...result, providerId: selectedProviderId, model: selectedCatalogModel.model });
    } catch (error) {
      setModelTest({
        providerId: selectedProviderId,
        model: selectedCatalogModel.model,
        httpStatus: null,
        latencyMs: 0,
        errorKeyword: errorText(error),
      });
    } finally {
      setTestingModelId("");
    }
  };

  const providerConfigured = selectedProvider?.configured ?? false;
  const providerStored = selectedProvider?.credentialSource === "stored";
  const providerFromEnvironment = selectedProvider?.credentialSource === "environment";
  const selectedProviderPersisted = Boolean(selectedBuiltInProvider) || Boolean(
    selectedProviderId && config?.customModelProviders?.some((provider) => provider.providerId === selectedProviderId),
  );
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
  const visibleOAuthSubscriptions = OAUTH_SUBSCRIPTIONS.filter((provider) =>
    providerMatches(provider.name, provider.models));
  const visibleApiProviders = (config?.modelProviders ?? []).filter((provider) => provider.builtIn &&
    providerMatches(provider.name, "API key HTTPS"));
  const providerPickerEmpty = !customCandidateVisible
    && visibleOAuthSubscriptions.length === 0
    && visibleApiProviders.length === 0;
  return (
    <div className="modelsDialogLayout">
      <aside className="modelsProviderList">
        {visibleBuiltIns.map((provider) => {
          const active = selectedProviderId === provider.providerId;
          return <div className="modelsProviderGroup" key={provider.providerId}>
            <button type="button" className={active && selection.kind === "provider" ? "modelsProviderButton is-active" : "modelsProviderButton"} onClick={() => setSelection({ kind: "provider", providerId: provider.providerId })}>
              <span>{provider.name}</span>{provider.configured ? <span className="modelsConfiguredDot" aria-label="configured" /> : null}
            </button>
          </div>;
        })}
        {visibleBuiltIns.length && customProviders.length ? <div className="modelsTreeDivider" /> : null}
        {customProviders.map((provider) => {
          const providerConfig = config?.modelProviders.find((item) => item.providerId === provider.providerId);
          const active = selectedProviderId === provider.providerId;
          return <div className="modelsProviderGroup" key={provider.providerId}>
            <button type="button" className={active && selection.kind === "provider" ? "modelsProviderButton is-active" : "modelsProviderButton"} onClick={() => setSelection({ kind: "provider", providerId: provider.providerId })}>
              <span>{provider.name}</span>{providerConfig?.configured ? <span className="modelsConfiguredDot" aria-label="configured" /> : null}
            </button>
            {providerConfig?.configured ? <div className="modelsProviderModels">{provider.models.map((model) => <button type="button" className={selection.kind === "model" && selection.providerId === provider.providerId && selection.modelKey === model.key ? "is-selected" : ""} key={model.key} onClick={() => setSelection({ kind: "model", providerId: provider.providerId, modelKey: model.key })}>{model.model || "new model"}</button>)}<button type="button" onClick={() => addModel(provider.providerId)}><Plus aria-hidden="true" />model</button></div> : null}
          </div>;
        })}
        <button type="button" className="modelsAddButton modelsAddProvider" onClick={() => { setProviderQuery(""); setProviderPickerOpen(true); }}><Plus aria-hidden="true" />Add provider</button>
      </aside>

      <section className="modelsEditor">
        <div className="modelsEditorScroll">
          {selection.kind === "empty" ? <div className="modelsEmptyState"><strong>Select a provider</strong><span>Choose an existing provider, or add a new one.</span></div> : null}

          {selection.kind === "provider" && selectedBuiltInProvider ? <section className="modelsSection">
            <div className="modelsSectionHeading"><span>API KEY</span><span className={providerConfigured ? "modelsCredentialStatus is-configured" : "modelsCredentialStatus"}><i />{providerConfigured ? "configured" : "not configured"}</span></div>
            <p className="modelsSectionDescription">{providerStored ? "Enter a new key to replace the stored key." : providerFromEnvironment ? "Credential is supplied by the current process environment." : "Enter an API key to add this provider."}</p>
            <div className="modelsCredentialRow"><div className="modelsKeyInput"><KeyRound aria-hidden="true" /><input type={revealedProviderId === selectedBuiltInProvider.providerId ? "text" : "password"} autoComplete="off" value={apiKeys[selectedBuiltInProvider.providerId] ?? ""} onChange={(event) => setApiKeys((keys) => ({ ...keys, [selectedBuiltInProvider.providerId]: event.target.value }))} placeholder={providerConfigured ? "Enter new key to replace…" : "sk-…"} /><button type="button" onClick={() => setRevealedProviderId((providerId) => providerId === selectedBuiltInProvider.providerId ? null : selectedBuiltInProvider.providerId)} aria-label="show API key">{revealedProviderId === selectedBuiltInProvider.providerId ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</button></div><button type="button" className="modelsLocalSave" disabled={Boolean(credentialProviderId) || !(apiKeys[selectedBuiltInProvider.providerId]?.trim())} onClick={() => void saveCredential(selectedBuiltInProvider.providerId)}>{credentialProviderId === selectedBuiltInProvider.providerId ? "Saving…" : "Save"}</button></div>
            {providerStored ? <button type="button" className="modelsDisconnect" disabled={Boolean(credentialProviderId)} onClick={() => void clearCredential(selectedBuiltInProvider.providerId)}>Remove credential</button> : null}
          </section> : null}

          {selection.kind === "provider" && selectedCustomProvider ? <section className="modelsSection">
            <div className="modelsSectionHeading"><span>PROVIDER</span><button type="button" className="modelsDangerButton" onClick={() => void removeSelectedProvider()}>Delete</button></div>
            <div className="modelsProviderForm">
              <label><span>Provider name</span><input value={selectedCustomProvider.name} onChange={(event) => updateCustomProvider({ name: event.target.value })} /></label>
              <label><span>Base URL</span><input value={selectedCustomProvider.baseUrl} onChange={(event) => updateCustomProvider({ baseUrl: event.target.value })} placeholder="https://api.example.com/v1" /></label>
              <label><span>API Key</span><div className="modelsCredentialRow"><div className="modelsKeyInput"><KeyRound aria-hidden="true" /><input type={revealedProviderId === selectedCustomProvider.providerId ? "text" : "password"} autoComplete="off" value={apiKeys[selectedCustomProvider.providerId] ?? ""} onChange={(event) => setApiKeys((keys) => ({ ...keys, [selectedCustomProvider.providerId]: event.target.value }))} placeholder={providerConfigured ? "Enter new key to replace…" : "API key"} /><button type="button" onClick={() => setRevealedProviderId((providerId) => providerId === selectedCustomProvider.providerId ? null : selectedCustomProvider.providerId)} aria-label="show API key">{revealedProviderId === selectedCustomProvider.providerId ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</button></div><button type="button" className="modelsLocalSave" disabled={!selectedProviderPersisted || Boolean(credentialProviderId) || !(apiKeys[selectedCustomProvider.providerId]?.trim())} onClick={() => void saveCredential(selectedCustomProvider.providerId)}>{credentialProviderId === selectedCustomProvider.providerId ? "Saving…" : "Save"}</button></div><small>{selectedProviderPersisted ? "Required before this provider's models become available." : "Save settings before storing a credential."}</small>{providerStored ? <button type="button" className="modelsDisconnect" disabled={Boolean(credentialProviderId)} onClick={() => void clearCredential(selectedCustomProvider.providerId)}>Remove credential</button> : null}</label>
              <label><span>API</span><select value={selectedCustomProvider.api} onChange={(event) => updateCustomProvider({ api: event.target.value as ModelWireApi })}>{API_OPTIONS.map((api) => <option key={api} value={api}>{api}</option>)}</select></label>
            </div>
          </section> : null}

          {selection.kind === "model" && selectedCustomModel ? <section className="modelsSection">
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
        </div>
        <footer className="modelsActions"><span className="resourceDialogMessage">{message || (loading ? "Loading…" : "")}</span>{canResetUnsupportedConfig ? <button type="button" className="modelsDangerButton" disabled={loading || savingSettings} onClick={() => void resetUnsupportedConfig()}>Reset configuration…</button> : null}<button type="button" onClick={onClose}>Cancel</button><button type="button" className="is-primary" onClick={() => void saveAll()} disabled={!config || loading || savingSettings}>{savingSettings ? "Saving…" : "Save"}</button></footer>
      </section>

      {providerPickerOpen ? <div className="modelsProviderPickerOverlay" role="presentation" onMouseDown={() => setProviderPickerOpen(false)}>
        <section className="modelsProviderPickerDialog" role="dialog" aria-modal="true" aria-label="Add provider" onMouseDown={(event) => event.stopPropagation()}>
          <header className="modelsProviderPickerHeader">
            <Search aria-hidden="true" />
            <input autoFocus aria-label="Search providers" placeholder="Search providers…" value={providerQuery} onChange={(event) => setProviderQuery(event.target.value)} />
            <button type="button" aria-label="Close provider catalog" onClick={() => setProviderPickerOpen(false)}><X aria-hidden="true" /></button>
          </header>
          <div className="modelsProviderPickerScroll">
            <div className="modelsProviderPicker">
              {customCandidateVisible ? <section><h2>CUSTOM</h2><div className="modelsProviderCards"><button type="button" onClick={addCustomProvider}><strong>OpenAI / Anthropic compatible</strong><span>Custom endpoint · HTTP or HTTPS</span><Plus aria-hidden="true" /></button></div></section> : null}
              {visibleOAuthSubscriptions.length ? <section><h2>OAUTH SUBSCRIPTIONS</h2><div className="modelsProviderCards">{visibleOAuthSubscriptions.map((provider) => <button type="button" disabled key={provider.name} title="OAuth provider SDK adapter is not enabled in this build"><strong>{provider.name}</strong><span>{provider.models}</span><small>SDK adapter pending</small></button>)}</div></section> : null}
              {visibleApiProviders.length ? <section><h2>API</h2><div className="modelsProviderCards">{visibleApiProviders.map((provider) => {
                return <button type="button" key={provider.providerId} onClick={() => { setSelection({ kind: "provider", providerId: provider.providerId }); setProviderPickerOpen(false); }}><strong>{provider.name}</strong></button>;
              })}</div></section> : null}
              {providerPickerEmpty ? <div className="modelsProviderPickerEmpty">No providers found.</div> : null}
            </div>
          </div>
        </section>
      </div> : null}

    </div>
  );
}

export default ModelsDialog;
