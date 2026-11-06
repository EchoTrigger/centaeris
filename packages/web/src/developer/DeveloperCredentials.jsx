import { useEffect, useState } from "react";
import { ApiError, apiJson, jsonOptions } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { useTranslation } from "../i18n";
import { CopyButton } from "./DeveloperExample";

const MAX_SCOPE_ITEMS = 64;

function focusNewInput(element) {
  element?.focus();
}

function toggleSelection(values, value) {
  return values.includes(value) ? values.filter(item => item !== value) : [...values, value];
}

function useCredentialCatalog(path, collection) {
  const [items, setItems] = useState(null);
  const [failed, setFailed] = useState(false);
  const [revision, setRevision] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Retry fetches only this catalog again.
  useEffect(() => {
    const controller = new AbortController();
    apiJson(path, { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setItems(result[collection]); })
      .catch(() => { if (!controller.signal.aborted) setFailed(true); });
    return () => controller.abort();
  }, [path, collection, revision]);

  function retry() {
    setItems(null);
    setFailed(false);
    setRevision(value => value + 1);
  }
  return { items, failed, retry };
}

function CatalogError({ message, onRetry }) {
  const { t } = useTranslation();
  return <p role="alert">{t(message)}{" "}
    <button className="secondary" type="button" onClick={onRetry}>{t("agentChat.retry")}</button>
  </p>;
}

function SourceFiles({ workspacePath, sourceId, selected, onToggle }) {
  const { t } = useTranslation();
  const [files, setFiles] = useState(null);
  const [failed, setFailed] = useState(false);
  const [revision, setRevision] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Retry fetches the same source again.
  useEffect(() => {
    const controller = new AbortController();
    apiJson(workspacePath + "/sources/" + encodeURIComponent(sourceId) + "/objects", { signal: controller.signal })
      .then(({ objects }) => {
        if (!controller.signal.aborted) setFiles(objects.filter(item => item.status === "ready" && item.objectType === "file"));
      })
      .catch(() => { if (!controller.signal.aborted) setFailed(true); });
    return () => controller.abort();
  }, [workspacePath, sourceId, revision]);

  if (failed) return <p role="alert">{t("developer.filesLoadFailed")}{" "}
    <button className="secondary" type="button" onClick={() => { setFailed(false); setRevision(value => value + 1); }}>{t("agentChat.retry")}</button>
  </p>;
  if (!files) return <p role="status">{t("agentChat.loading")}</p>;
  if (!files.length) return <p>{t("developer.noFiles")}</p>;
  return <div className="developerScopeOptions">{files.map(file => <label key={file.id}>
    <input type="checkbox" checked={selected.includes(file.id)} disabled={!selected.includes(file.id) && selected.length >= MAX_SCOPE_ITEMS}
      onChange={() => onToggle(file.id)} />
    {file.displayPath || file.displayName}
  </label>)}</div>;
}

function CredentialScopes({ workspacePath, pluginNames, setPlugins, sourceObjectRefs, setSourceObjects }) {
  const { t } = useTranslation();
  const plugins = useCredentialCatalog(workspacePath + "/plugins", "plugins");
  const sources = useCredentialCatalog(workspacePath + "/sources", "sources");
  const [sourceId, setSource] = useState("");
  const enabledPlugins = plugins.items?.filter(item => item.enabled);
  const readySources = sources.items?.filter(item => item.status === "ready");

  return <>
    <fieldset>
      <legend>{t("developer.plugins")}</legend>
      {plugins.failed ? <CatalogError message="developer.pluginsLoadFailed" onRetry={plugins.retry} />
        : !enabledPlugins ? <p role="status">{t("agentChat.loading")}</p>
        : enabledPlugins.length ? <div className="developerScopeOptions">{enabledPlugins.map(plugin => <label key={plugin.name}>
          <input type="checkbox" checked={pluginNames.includes(plugin.name)} disabled={!pluginNames.includes(plugin.name) && pluginNames.length >= MAX_SCOPE_ITEMS}
            onChange={() => setPlugins(values => toggleSelection(values, plugin.name))} />{plugin.displayName}
        </label>)}</div> : <p>{t("developer.noPlugins")}</p>}
    </fieldset>
    <fieldset>
      <legend>{t("developer.materials")}</legend>
      {sources.failed ? <CatalogError message="developer.sourcesLoadFailed" onRetry={sources.retry} />
        : !readySources ? <p role="status">{t("agentChat.loading")}</p>
        : readySources.length ? <>
          <label className="field">{t("developer.source")}
            <select value={sourceId} onChange={event => setSource(event.target.value)}>
              <option value="">{t("developer.chooseSource")}</option>
              {readySources.map(source => <option key={source.id} value={source.id}>{source.name}</option>)}
            </select>
          </label>
          {sourceId ? <SourceFiles key={sourceId} workspacePath={workspacePath} sourceId={sourceId}
            selected={sourceObjectRefs} onToggle={id => setSourceObjects(values => toggleSelection(values, id))} /> : null}
          <p>{t("developer.filesSelected", { count: sourceObjectRefs.length })}{" "}
            {sourceObjectRefs.length ? <button type="button" className="secondary" onClick={() => setSourceObjects([])}>{t("developer.clearFiles")}</button> : null}
          </p>
        </> : <p>{t("developer.noSources")}</p>}
    </fieldset>
  </>;
}

function CredentialForm({ workspaceId, workspacePath, busy, onCreate, onClose }) {
  const { t } = useTranslation();
  const models = useCredentialCatalog("/api/models?" + new URLSearchParams({ workspaceId }), "models");
  const [name, setName] = useState("");
  const [modelConfigRef, setModel] = useState("");
  const [pluginNames, setPlugins] = useState([]);
  const [sourceObjectRefs, setSourceObjects] = useState([]);
  const [scopesRequested, setScopesRequested] = useState(false);

  const validModel = models.items?.some(item => item.id === modelConfigRef);
  return <form className="developerCredentialForm" onSubmit={event => {
    event.preventDefault();
    if (validModel && name.trim() && !busy) void onCreate({ name: name.trim(), modelConfigRef, pluginNames, sourceObjectRefs });
  }}>
    <fieldset disabled={busy}>
      <legend>{t("developer.createCredential")}</legend>
      <label className="field">{t("developer.credentialName")}
        <input ref={focusNewInput} required maxLength={120} value={name} onChange={event => setName(event.target.value)} />
      </label>
      <label className="field">{t("developer.model")}
        <select required value={validModel ? modelConfigRef : ""} disabled={!models.items?.length}
          onChange={event => setModel(event.target.value)}>
          <option value="">{t("developer.chooseModel")}</option>
          {models.items?.map(model => <option key={model.id} value={model.id}>
            {model.providerDisplayName ? model.providerDisplayName + " / " : ""}{model.displayName}
          </option>)}
        </select>
      </label>
      {models.failed ? <CatalogError message="developer.modelsLoadFailed" onRetry={models.retry} />
        : !models.items ? <p role="status">{t("agentChat.loading")}</p>
        : models.items.length === 0 ? <p>{t("developer.noModels")}</p> : null}
      <details className="developerOptionalScopes" onToggle={event => {
        if (event.currentTarget.open) setScopesRequested(true);
      }}>
        <summary>{t("developer.optionalScopes")}</summary>
        {scopesRequested ? <CredentialScopes workspacePath={workspacePath} pluginNames={pluginNames} setPlugins={setPlugins}
          sourceObjectRefs={sourceObjectRefs} setSourceObjects={setSourceObjects} /> : null}
      </details>
      <div className="developerActions">
        <button type="button" className="secondary" onClick={onClose}>{t("agentRunRow.cancel")}</button>
        <button type="submit" className="primary" disabled={!name.trim() || !validModel}>{t("developer.createCredential")}</button>
      </div>
    </fieldset>
  </form>;
}

function credentialErrorKey(error, action) {
  if (error instanceof ApiError && error.message === "csrf_failed") return "developer.csrfFailed";
  if (error instanceof ApiError && error.status === 403) return "developer.permissionChanged";
  if (action === "revoke") return "developer.revokeFailed";
  if (error instanceof ApiError) {
    if (error.message === "model_not_found") return "developer.modelUnavailable";
    if (error.message === "api_credential_plugin_unavailable") return "developer.pluginUnavailable";
    if (error.message === "api_credential_source_not_found") return "developer.sourceUnavailable";
    if (error.status === 400 || error.status === 422) return "developer.invalidCredential";
  }
  return "developer.createFailed";
}

export function DeveloperCredentials({ workspaceId }) {
  const { t } = useTranslation();
  const workspacePath = "/api/workspaces/" + encodeURIComponent(workspaceId);
  const path = workspacePath + "/api-credentials";
  const [credentials, setCredentials] = useState(null);
  const [listFailed, setListFailed] = useState(false);
  const [revision, setRevision] = useState(0);
  const [creating, setCreating] = useState(false);
  const [issued, setIssued] = useState(null);
  const [issuedSecrets, setIssuedSecrets] = useState({});
  const [pendingRevoke, setPendingRevoke] = useState(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // biome-ignore lint/correctness/useExhaustiveDependencies: Refresh reconciles the list after an uncertain request.
  useEffect(() => {
    const controller = new AbortController();
    apiJson(path, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setCredentials(result.credentials);
    }).catch(() => { if (!controller.signal.aborted) setListFailed(true); });
    return () => controller.abort();
  }, [path, revision]);

  function reload() {
    setListFailed(false);
    setCredentials(null);
    setRevision(value => value + 1);
  }

  async function create(payload) {
    setError("");
    setBusy(true);
    try {
      const result = await apiJson(path, jsonOptions("POST", payload));
      setCredentials(values => [...values, result.credential]);
      setIssued(result.credential);
      setIssuedSecrets(values => ({ ...values, [result.credential.id]: result.secret }));
      setCreating(false);
    } catch (error) {
      setError(credentialErrorKey(error, "create"));
    } finally {
      setBusy(false);
    }
  }

  async function revoke() {
    setError("");
    setBusy(true);
    try {
      const { credential } = await apiJson(path + "/" + encodeURIComponent(pendingRevoke.id), { method: "DELETE" });
      setCredentials(values => values.map(item => item.id === credential.id ? credential : item));
      setIssuedSecrets(values => {
        const remaining = { ...values };
        delete remaining[credential.id];
        return remaining;
      });
      setPendingRevoke(null);
    } catch (error) {
      setError(credentialErrorKey(error, "revoke"));
      setPendingRevoke(null);
    } finally {
      setBusy(false);
    }
  }

  return <section className="settingsPanel developerSection" aria-labelledby="developer-credentials-heading">
    <div className="developerSectionHead">
      <div>
        <h2 id="developer-credentials-heading">{t("developer.credentials")}</h2>
        <p>{t("developer.credentialsHint")}</p>
      </div>
      {credentials && !creating && !issued ? <button type="button" className="primary" disabled={busy}
        onClick={() => { setError(""); setCreating(true); }}>{t("developer.createCredential")}</button> : null}
    </div>
    {error ? <p role="alert">{t(error)}{" "}
      {error === "developer.createFailed" || error === "developer.revokeFailed" ? <button type="button" className="secondary"
        onClick={() => { setCreating(false); setError(""); reload(); }}>{t("developer.refreshCredentials")}</button> : null}
    </p> : null}
    {issued ? <section className="developerIssued" aria-labelledby="developer-issued-heading">
      <h3 id="developer-issued-heading">{t("developer.credentialCreated", { name: issued.name })}</h3>
      <p>{t("developer.secretCopyHint")}</p>
      <label className="field">{t("developer.secret")}
        <input ref={focusNewInput} type="text" readOnly autoComplete="off" spellCheck={false} value={issuedSecrets[issued.id]} onFocus={event => event.currentTarget.select()} />
      </label>
      <div className="developerActions">
        <CopyButton value={issuedSecrets[issued.id]} label={t("developer.copySecret")} />
        <button type="button" className="primary" onClick={() => setIssued(null)}>{t("developer.done")}</button>
      </div>
    </section> : null}
    {creating ? <CredentialForm workspaceId={workspaceId} workspacePath={workspacePath} busy={busy || error === "developer.createFailed"} onCreate={create}
      onClose={() => { setCreating(false); setError(""); }} /> : null}
    {listFailed ? <p role="alert">{t("developer.credentialsLoadFailed")}{" "}
      <button type="button" className="secondary" onClick={reload}>{t("agentChat.retry")}</button>
    </p> : credentials === null ? <p role="status">{t("agentChat.loading")}</p> : credentials.length === 0
      ? <p>{t("developer.noCredentials")}</p>
      : <ul className="developerCredentialList">{credentials.map(item => <li key={item.id}>
        <div>
          <strong>{item.name}</strong>
          <p><code>{item.modelConfigRef}</code></p>
          <p>{t("developer.scopeCounts", { plugins: item.pluginNames.length, files: item.sourceObjectRefs.length })}</p>
          {item.pluginNames.length || item.sourceObjectRefs.length ? <details className="developerScopeDetails">
            <summary>{t("developer.grantedPermissions")}</summary>
            {item.pluginNames.length ? <p>{t("developer.plugins")}{" "}<code>{item.pluginNames.join(", ")}</code></p> : null}
            {item.sourceObjectRefs.length ? <p>{t("developer.fileReferences")}{" "}<code>{item.sourceObjectRefs.join(", ")}</code></p> : null}
          </details> : null}
        </div>
        {item.revokedAt ? <span>{t("developer.revoked")}</span> : <div className="developerActions">
          {!issued && issuedSecrets[item.id] ? <CopyButton value={issuedSecrets[item.id]} label={t("developer.copySecret")}
            accessibleLabel={t("developer.copySecretNamed", { name: item.name })} /> : null}
          <button className="secondary" type="button" disabled={busy || creating || Boolean(issued)}
            aria-label={t("developer.revokeNamed", { name: item.name })} onClick={() => setPendingRevoke(item)}>{t("developer.revoke")}</button>
        </div>}
      </li>)}</ul>}
    <ConfirmDialog open={Boolean(pendingRevoke)} title={t("developer.revokeNamed", { name: pendingRevoke?.name })}
      message={t("developer.revokeHint")} confirmLabel={t("developer.revoke")} busy={busy}
      onCancel={() => setPendingRevoke(null)} onConfirm={() => void revoke()} />
  </section>;
}
