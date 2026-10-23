import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { ArrowLeft, Check, CircleAlert, Eye, EyeOff, KeyRound, LoaderCircle, MoreHorizontal, Plus, Search, Server, Trash2, X, ChevronRight } from "lucide-react";
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
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Checkbox } from "./ui/checkbox";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./ui/dropdown-menu";
import { Input } from "./ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";
import "../styles/desktop-settings.css";
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

const customDraftsFromConfig = (config: AgentRuntimeConfig, currentDrafts?: CustomProviderDraft[]): CustomProviderDraft[] =>
  (config.customModelProviders ?? []).map((provider) => ({
    providerId: provider.providerId,
    name: provider.name,
    baseUrl: provider.baseUrl,
    api: provider.api,
    models: provider.models.map((model, index) => ({
        // Editing only appends models; preserve existing row identities when cancelling.
        key: currentDrafts?.find(draft => draft.providerId === provider.providerId)?.models[index]?.key ?? nextCustomModelKey++,
        model: model.model,
        displayName: model.displayName ?? "",
        contextTokens: model.contextTokens,
        maxOutputTokens: model.maxOutputTokens,
        apiOverride: model.apiOverride,
        supportsVision: model.supportsVision,
      })),
  }));

export const buildCustomProvidersInput = (
  providers: readonly CustomModelProviderInput[],
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

function SettingsDialog({ title, description, children, onClose, busy = false, initialFocus }: {
  title: string; description?: string; children: ReactNode; onClose: () => void; busy?: boolean;
  initialFocus?: RefObject<HTMLInputElement | null>;
}) {
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose(); }}>
    <DialogContent className="desktopModelDialog" showCloseButton={false} initialFocus={initialFocus}>
      <DialogHeader><DialogTitle>{title}</DialogTitle>{description ? <DialogDescription>{description}</DialogDescription> : null}</DialogHeader>
      <DialogClose render={<Button variant="ghost" size="icon-sm" className="desktopModelDialogClose" aria-label="Close detail" disabled={busy} />}><X size={16} /></DialogClose>
      {children}
    </DialogContent>
  </Dialog>;
}

function ProtocolSelect({ value, onChange, inherit = false, disabled = false }: {
  value: string; onChange: (value: string) => void; inherit?: boolean; disabled?: boolean;
}) {
  return <Select value={value} onValueChange={next => { if (next !== null) onChange(next); }} disabled={disabled}>
    <SelectTrigger aria-label={inherit ? "API override" : "API protocol"}><SelectValue>{value === "inherit" ? "Inherit provider" : value}</SelectValue></SelectTrigger>
    <SelectContent align="start" alignItemWithTrigger={false}>
      {inherit ? <SelectItem value="inherit">Inherit provider</SelectItem> : null}
      {API_OPTIONS.map(api => <SelectItem key={api} value={api}>{api}</SelectItem>)}
    </SelectContent>
  </Select>;
}

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
  const dialogInput = useRef<HTMLInputElement>(null);
  const screenOwner = useRef({});
  const testOwner = useRef<object | null>(null);
  const invalidateTest = useCallback(() => {
    testOwner.current = null;
    setTestingModelId("");
    setModelTest(null);
  }, []);
  const select = useCallback((next: Selection) => {
    setSelection(next);
    invalidateTest();
  }, [invalidateTest]);

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
    const owner = screenOwner.current;
    setSavingSettings(true);
    try {
      const confirmed = await confirmAction({
        title: "Reset model settings?",
        message: "This removes all API keys saved by Centaeris. Environment credentials stay unchanged.",
      });
      if (!confirmed || owner !== screenOwner.current) return;
      setMessage("");
      const response = await resetAgentRuntimeConfig();
      if (owner !== screenOwner.current) return;
      acceptConfig(response.config);
      setCustomProviders([]);
      setApiKeys({});
      select({ kind: "empty" });
      setCanResetUnsupportedConfig(false);
      setMessage("Runtime configuration reset");
    } catch (error) {
      if (owner === screenOwner.current) setMessage(errorText(error));
    } finally {
      if (owner === screenOwner.current) setSavingSettings(false);
    }
  };

  useEffect(() => {
    void load();
    return () => { loadSequence.current += 1; };
  }, [load]);
  useEffect(() => () => {
    screenOwner.current = {};
    testOwner.current = null;
  }, []);

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
    invalidateTest();
  };

  const cancelEdit = () => {
    if (savingSettings || credentialProviderId) return;
    if (config) setCustomProviders(customDraftsFromConfig(config, customProviders));
    setApiKeys({}); setRevealedProviderId(null); setEditing(false); setMessage("");
    select(selectedProviderId && config?.modelProviders.some(p => p.providerId === selectedProviderId)
      ? {kind:"provider",providerId:selectedProviderId} : {kind:"empty"});
  };
  const savedProviders = useMemo(() => JSON.stringify(buildCustomProvidersInput(config?.customModelProviders ?? [])), [config]);
  const settingsDirty = savedProviders !== JSON.stringify(buildCustomProvidersInput(customProviders));
  const hasChanges = settingsDirty || Boolean(apiKeys[selectedProviderId ?? ""]?.trim());

  const saveAll = async () => {
    if (loading || savingSettings || credentialProviderId || !selectedProviderId || !hasChanges) return;
    const owner = screenOwner.current;
    setSavingSettings(true); setMessage("");
    let settingsSaved = false;
    try {
      if (selectedCustomProvider) {
        const next = await setAgentRuntimeConfig({customModelProviders: buildCustomProvidersInput(customProviders)});
        if (owner !== screenOwner.current) return;
        acceptConfig(next); settingsSaved = true;
      }
      const key = apiKeys[selectedProviderId]?.trim();
      if (key) {
        const next = await setAgentRuntimeConfig({modelProviderId:selectedProviderId,modelApiKey:key});
        if (owner !== screenOwner.current) return;
        acceptConfig(next);
        setApiKeys({});
      }
      setEditing(false); setRevealedProviderId(null);
      select({kind:"provider",providerId:selectedProviderId});
      setMessage("Saved");
    } catch (error) {
      if (owner !== screenOwner.current) return;
      setMessage(`${settingsSaved ? "Connection settings saved, but the API key was not saved. Retry saving the key. " : ""}${errorText(error)}`);
    } finally { if (owner === screenOwner.current) setSavingSettings(false); }
  };

  const clearCredential = async (providerId: string) => {
    if (credentialProviderId || savingSettings || loading) return;
    const owner = screenOwner.current;
    invalidateTest();
    setCredentialProviderId(providerId);
    setMessage("");
    try {
      const next = await setAgentRuntimeConfig({
        modelProviderId: providerId,
        clearModelApiKey: true,
      });
      if (owner !== screenOwner.current) return;
      acceptConfig(next);
      select({ kind: "provider", providerId });
      setMessage("Credential removed");
    } catch (error) {
      if (owner === screenOwner.current) setMessage(errorText(error));
    } finally {
      if (owner === screenOwner.current) setCredentialProviderId("");
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
    if (!selectedCustomProvider || !selectedCustomModel || savingSettings || credentialProviderId) return;
    const owner = screenOwner.current;
    setSavingSettings(true);
    invalidateTest();
    setMessage("");
    try {
      const confirmed = await confirmAction({
        title: "Remove this model?",
        message: `“${selectedCustomModel.model || "New model"}” will be removed from this provider.`,
      });
      if (!confirmed || owner !== screenOwner.current) return;
      const providers = customProviders.map(provider => provider.providerId !== selectedCustomProvider.providerId ? provider : { ...provider, models: provider.models.filter(model => model.key !== selectedCustomModel.key) });
      const next = await setAgentRuntimeConfig({ customModelProviders: buildCustomProvidersInput(providers) });
      if (owner !== screenOwner.current) return;
      acceptConfig(next);
      setCustomProviders(customDraftsFromConfig(next));
      setEditing(false);
      select({ kind: "provider", providerId: selectedCustomProvider.providerId });
      setMessage("Model removed");
    } catch (error) {
      if (owner === screenOwner.current) setMessage(errorText(error));
    } finally {
      if (owner === screenOwner.current) setSavingSettings(false);
    }
  };

  const removeSelectedProvider = async () => {
    if (!selectedCustomProvider || savingSettings || credentialProviderId) return;
    const owner = screenOwner.current;
    setSavingSettings(true);
    invalidateTest();
    setMessage("");
    try {
      const confirmed = await confirmAction({
        title: "Remove this provider?",
        message: `“${selectedCustomProvider.name}” and its models will be removed.`,
      });
      if (!confirmed || owner !== screenOwner.current) return;
      const next = await setAgentRuntimeConfig({ customModelProviders: buildCustomProvidersInput(customProviders.filter(provider => provider.providerId !== selectedCustomProvider.providerId)) });
      if (owner !== screenOwner.current) return;
      acceptConfig(next);
      setCustomProviders(customDraftsFromConfig(next));
      setEditing(false);
      select({ kind: "empty" });
      setMessage("Service removed");
    } catch (error) {
      if (owner === screenOwner.current) setMessage(errorText(error));
    } finally {
      if (owner === screenOwner.current) setSavingSettings(false);
    }
  };

  const runTest = async (modelId = selectedModelId) => {
    const catalogModel = selectedProvider?.models.find(model => model.model === modelId);
    if (!catalogModel || !selectedProviderId || catalogModel.diagnostic || testingModelId) return;
    const testId = `${selectedProviderId}\0${catalogModel.model}`;
    const owner = {};
    testOwner.current = owner;
    setTestingModelId(testId);
    setMessage("");
    try {
      const result = await testAgentRuntimeModel({ providerId: selectedProviderId, model: catalogModel.model });
      if (testOwner.current !== owner) return;
      setModelTest({ ...result, providerId: selectedProviderId, model: catalogModel.model });
    } catch (error) {
      if (testOwner.current !== owner) return;
      setModelTest({
        providerId: selectedProviderId,
        model: catalogModel.model,
        httpStatus: null,
        latencyMs: 0,
        errorKeyword: errorText(error),
      });
    } finally {
      if (testOwner.current === owner) {
        testOwner.current = null;
        setTestingModelId("");
      }
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
  const visibleModels = modelRows.filter(model => `${model.id} ${model.name}`.toLocaleLowerCase().includes(modelQuery.trim().toLocaleLowerCase()));
  const providerName = selectedCustomProvider?.name ?? selectedProvider?.name ?? "New provider";
  const saveButtons = <footer className="desktopModelEditActions">
    <span>{hasChanges ? "Unsaved changes" : "No changes"}</span>
    <Button variant="ghost" disabled={busy} onClick={cancelEdit}>Cancel</Button>
    <Button variant="solid" disabled={busy || !config || !hasChanges} onClick={() => void saveAll()}>{savingSettings ? <LoaderCircle size={15} className="desktopSettingsSpinner" /> : null}{savingSettings ? "Saving…" : "Save"}</Button>
  </footer>;
  const messageSucceeded = ["Saved", "Credential removed", "Runtime configuration reset", "Model removed", "Service removed"].includes(message);
  const feedback = message ? <div className="desktopModelFeedback" data-success={messageSucceeded} role="status">
    {messageSucceeded ? <Check size={16} /> : <CircleAlert size={16} />}<span>{message}</span>
  </div> : null;
  const isTestingSelectedModel = Boolean(testingModelId) && testingModelId === selectedTestId;
  const selectedResult = testForSelectedModel ? <p className="desktopModelTestResult" data-success={testSucceeded(testForSelectedModel)} role="status">{testSucceeded(testForSelectedModel) ? <Check size={16} /> : <CircleAlert size={16} />}<span>{testSummary(testForSelectedModel)}</span></p> : null;
  return <div className="desktopModels" aria-busy={loading || busy}>
    {selection.kind === "empty" ? <>
      <header className="desktopSettingsHeading desktopModelsHeading"><div><h1>Model services</h1></div><Button variant="outline" disabled={!config || loading} onClick={() => { setProviderQuery(""); setProviderPickerOpen(true); }}><Plus size={16} />Add service</Button></header>
      {listedProviders.length ? <section className="desktopModelServiceList" aria-label="Configured model services">{listedProviders.map(provider => <Button variant="ghost" className="desktopModelServiceRow" key={provider.providerId} aria-label={`Open ${provider.name}`} onClick={() => openProvider(provider.providerId)}>
        <ProviderLogo svg={provider.logoSvg} name={provider.name} /><span className="desktopModelServiceIdentity"><strong>{provider.name}</strong><small>{credentialLabel(provider)}</small></span><Badge variant="secondary">{provider.models.length} {provider.models.length === 1 ? "model" : "models"}</Badge><ChevronRight size={16} />
      </Button>)}</section> : !loading && config ? <div className="desktopModelEmpty"><Server size={24} /><h2>No model services</h2><Button variant="outline" onClick={() => { setProviderQuery(""); setProviderPickerOpen(true); }}><Plus size={16} />Choose a provider</Button></div> : null}
    </> : <>
      <nav className="desktopModelBreadcrumb" aria-label="Model service navigation"><Button variant="ghost" size="sm" disabled={editing || busy} onClick={() => { select({ kind: "empty" }); setMessage(""); }}><ArrowLeft size={14} />Model services</Button><span>/</span><span>{providerName}</span></nav>
      <header className="desktopSettingsHeading desktopModelsHeading"><div><h1>{providerName}</h1></div>
        {!editing && (providerStored || selectedCustomProvider) ? <DropdownMenu><DropdownMenuTrigger render={<Button variant="outline" size="icon" aria-label="Service actions" disabled={busy} />}><MoreHorizontal size={18} /></DropdownMenuTrigger><DropdownMenuContent align="start">
          {providerStored ? <DropdownMenuItem disabled={busy} onClick={() => void clearCredential(selectedProviderId!)}><KeyRound size={15} />Remove credential</DropdownMenuItem> : null}
          {selectedCustomProvider ? <DropdownMenuItem variant="destructive" disabled={busy} onClick={() => void removeSelectedProvider()}><Trash2 size={15} />Remove service</DropdownMenuItem> : null}
        </DropdownMenuContent></DropdownMenu> : null}
      </header>
      <section className="desktopModelSection" aria-labelledby="modelConnectionHeading">
        <header className="desktopModelSectionHeading"><div><h2 id="modelConnectionHeading">Connection</h2></div>{!editing ? <Button variant="outline" disabled={busy} onClick={() => { setMessage(""); setEditing(true); }}>Edit connection</Button> : null}</header>
        {editing && selection.kind === "provider" ? <>
          <div className="desktopModelForm">
            {selectedCustomProvider ? <><label className="desktopModelField">Provider name<Input aria-label="Provider name" value={selectedCustomProvider.name} disabled={busy} onChange={event => updateCustomProvider({ name: event.target.value })} /></label>
              <label className="desktopModelField">Base URL<Input value={selectedCustomProvider.baseUrl} disabled={busy} onChange={event => updateCustomProvider({ baseUrl: event.target.value })} placeholder="https://api.example.com/v1" /></label>
              <div className="desktopModelField"><span>API protocol</span><ProtocolSelect value={selectedCustomProvider.api} disabled={busy} onChange={api => updateCustomProvider({ api: api as ModelWireApi })} /></div></> : null}
            <label className="desktopModelField">API key<div className="desktopModelKeyInput"><Input aria-label="API key" autoComplete="off" spellCheck={false} disabled={busy} type={revealedProviderId === selectedProviderId ? "text" : "password"} value={apiKeys[selectedProviderId!] ?? ""} onChange={event => setApiKeys(keys => ({ ...keys, [selectedProviderId!]: event.target.value }))} placeholder={selectedProvider?.configured ? "Leave blank to keep current credential" : "Enter API key"} /><Button variant="ghost" size="icon-sm" disabled={busy} aria-label="Toggle API key visibility" aria-pressed={revealedProviderId === selectedProviderId} onClick={() => setRevealedProviderId(revealedProviderId ? null : selectedProviderId)}>{revealedProviderId ? <EyeOff size={16} /> : <Eye size={16} />}</Button></div><small>Stored locally. Blank keeps the current credential.</small></label>
          </div>{saveButtons}
        </> : <dl className="desktopModelFacts"><div><dt>Credential</dt><dd><KeyRound size={14} />{credentialLabel(selectedProvider)}</dd></div>{selectedCustomProvider ? <><div><dt>Base URL</dt><dd>{selectedCustomProvider.baseUrl || "No endpoint configured"}</dd></div><div><dt>API protocol</dt><dd>{selectedCustomProvider.api}</dd></div></> : null}</dl>}
      </section>
      {!editing || selection.kind === "model" ? <section className="desktopModelSection" aria-labelledby="modelListHeading"><header className="desktopModelSectionHeading"><div><h2 id="modelListHeading">Models <span className="desktopModelCount">{modelRows.length}</span></h2></div>{selectedCustomProvider ? <Button variant="outline" disabled={busy} onClick={() => addModel(selectedProviderId!)}><Plus size={16} />Add model</Button> : null}</header>
        {modelRows.length ? <div className="desktopModelSearch"><Search size={15} /><Input aria-label="Search models" placeholder="Search models" value={modelQuery} onChange={event => setModelQuery(event.target.value)} /></div> : null}
        <div className="desktopModelRows">{visibleModels.map(model => {
          const catalogModel = selectedProvider?.models.find(item => item.model === model.id);
          return <div className="desktopModelRow" key={model.key}><Button variant="ghost" className="desktopModelOpen" disabled={busy} aria-label={model.name || "New model"} onClick={() => { setMessage(""); setEditing(true); select({ kind: "model", providerId: selectedProviderId!, modelKey: model.key }); }}><span><strong>{model.name || "New model"}</strong>{model.name !== model.id ? <small>{model.id}</small> : null}</span>{catalogModel?.diagnostic ? <Badge variant="outline">Needs attention</Badge> : null}<ChevronRight size={15} /></Button><Button variant="outline" size="sm" aria-label={`Test ${model.id}`} disabled={busy || Boolean(testingModelId) || settingsDirty || !catalogModel || Boolean(catalogModel.diagnostic)} onClick={() => void runTest(model.id)}>{testingModelId === `${selectedProviderId}\0${model.id}` ? <LoaderCircle size={14} className="desktopSettingsSpinner" /> : null}{testingModelId === `${selectedProviderId}\0${model.id}` ? "Testing…" : "Test"}</Button>{modelTest?.providerId === selectedProviderId && modelTest.model === model.id && selection.kind === "provider" ? <p className="desktopModelTestResult" data-success={testSucceeded(modelTest)} role="status">{testSucceeded(modelTest) ? <Check size={16} /> : <CircleAlert size={16} />}<span>{modelTest.model}: {testSummary(modelTest)}</span></p> : null}</div>;
        })}</div>
        {!modelRows.length ? <div className="desktopModelEmpty desktopModelEmpty--compact"><h3>No models added</h3>{selectedCustomProvider ? <Button variant="outline" disabled={busy} onClick={() => addModel(selectedProviderId!)}>Add a model</Button> : null}</div> : !visibleModels.length ? <div className="desktopModelEmpty desktopModelEmpty--compact"><p>No models match “{modelQuery}”.</p><Button variant="ghost" onClick={() => setModelQuery("")}>Clear search</Button></div> : null}
      </section> : null}
    </>}
    {busy && !editing ? <div className="desktopModelLoading" role="status"><LoaderCircle size={16} className="desktopSettingsSpinner" />Updating service…</div> : null}
    {loading ? <div className="desktopModelLoading" role="status"><LoaderCircle size={16} className="desktopSettingsSpinner" />Loading model services…</div> : null}
    {selection.kind !== "model" ? feedback : null}
    {!loading && !config ? <div className="desktopModelRecovery"><Button variant="outline" disabled={busy} onClick={() => void load()}>Retry</Button>{canResetUnsupportedConfig ? <Button variant="destructive" disabled={busy} onClick={() => void resetUnsupportedConfig()}>Reset configuration…</Button> : null}</div> : null}
    {selection.kind === "model" ? <SettingsDialog title={selectedModelId || "New model"} description={providerName} onClose={cancelEdit} busy={busy} initialFocus={selectedCustomModel ? dialogInput : undefined}>
      <div className="desktopModelDialogToolbar"><span>{selectedCustomModel ? <Badge variant="secondary">{hasChanges ? "Unsaved changes" : "Custom model"}</Badge> : <Badge variant="secondary">Provider model</Badge>}</span><Button variant="outline" disabled={busy || settingsDirty || !selectedCatalogModel || Boolean(selectedCatalogModel.diagnostic) || Boolean(testingModelId)} onClick={() => void runTest()}>{isTestingSelectedModel ? <LoaderCircle size={15} className="desktopSettingsSpinner" /> : null}{isTestingSelectedModel ? "Testing…" : "Test"}</Button></div>
      {selectedCatalogModel?.diagnostic ? <p className="desktopModelDiagnostic"><CircleAlert size={16} /><span>{selectedCatalogModel.diagnostic}</span></p> : null}
      {selectedResult}
      {selectedCustomModel ? <><div className="desktopModelForm">
        <label className="desktopModelField">ID *<Input ref={dialogInput} value={selectedCustomModel.model} disabled={busy} onChange={event => updateCustomModel(selectedCustomModel.key, { model: event.target.value })} placeholder="model-id" /><small>Exact model ID from this provider.</small></label>
        <label className="desktopModelField">Name<Input value={selectedCustomModel.displayName} disabled={busy} onChange={event => updateCustomModel(selectedCustomModel.key, { displayName: event.target.value })} placeholder="Optional display name" /></label>
        <div className="desktopModelField"><span>API override</span><ProtocolSelect value={selectedCustomModel.apiOverride ?? "inherit"} disabled={busy} inherit onChange={api => updateCustomModel(selectedCustomModel.key, { apiOverride: api === "inherit" ? undefined : api as ModelWireApi })} /></div>
        <div className="desktopModelFormColumns"><label className="desktopModelField">Context window<Input value={selectedCustomModel.contextTokens} disabled={busy} onChange={event => updateCustomModel(selectedCustomModel.key, { contextTokens: event.target.value })} placeholder="128k" /></label><label className="desktopModelField">Max output<Input value={selectedCustomModel.maxOutputTokens} disabled={busy} onChange={event => updateCustomModel(selectedCustomModel.key, { maxOutputTokens: event.target.value })} placeholder="32k" /></label></div>
        <label className="desktopModelCheckbox"><Checkbox aria-label="Image input" checked={selectedCustomModel.supportsVision} disabled={busy} onCheckedChange={checked => updateCustomModel(selectedCustomModel.key, { supportsVision: checked })} /><span><strong>Image input</strong></span></label>
      </div>{settingsDirty ? <p className="desktopSettingsNote">Save changes before testing this model.</p> : null}
      {feedback}<div className="desktopModelDialogFooter"><Button variant="ghost" className="desktopModelRemove" disabled={busy} onClick={() => void removeSelectedModel()}><Trash2 size={15} />Remove model</Button>{saveButtons}</div></> : feedback}
    </SettingsDialog> : null}
    {providerPickerOpen ? <SettingsDialog title="Add service" onClose={() => setProviderPickerOpen(false)} initialFocus={dialogInput}>
      <div className="desktopModelSearch"><Search size={16} /><Input ref={dialogInput} aria-label="Search providers" placeholder="Search services" value={providerQuery} onChange={event => setProviderQuery(event.target.value)} /></div>
      <div className="desktopModelPicker">{customCandidateVisible ? <Button variant="ghost" className="desktopModelPickerRow" onClick={addCustomProvider}><span className="desktopModelProviderIcon"><Plus size={18} /></span><span><strong>Custom service</strong><small>OpenAI / Anthropic compatible</small></span><ChevronRight size={16} /></Button> : null}
        {visibleApiProviders.map(provider => <Button variant="ghost" className="desktopModelPickerRow" key={provider.providerId} onClick={() => { openProvider(provider.providerId); setEditing(true); setProviderPickerOpen(false); }}><ProviderLogo svg={provider.logoSvg} name={provider.name} /><span><strong>{provider.name}</strong><small>{credentialLabel(provider)}</small></span><ChevronRight size={16} /></Button>)}
        {providerPickerEmpty ? <div className="desktopModelEmpty desktopModelEmpty--compact"><p>No services found.</p><Button variant="ghost" onClick={() => setProviderQuery("")}>Clear search</Button></div> : null}
      </div>
    </SettingsDialog> : null}
  </div>;
}

export default ModelsDialog;
