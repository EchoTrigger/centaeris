import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Blocks, ChevronRight, Upload } from "lucide-react";
import { Switch } from "@centaeris/frontend-foundations/switch";
import { apiJson, jsonOptions } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";

function errorText(error) {
  return error instanceof Error ? error.message : String(error);
}

function credentialKey(pluginName, credentialRef) {
  return `${pluginName}:${credentialRef}`;
}

function pluginErrors(plugin) {
  const messages = {
    plugin_manifest_invalid: t("pluginSettings.thePluginFilesOrDescriptionAreInvalidUpdateOr"),
    plugin_credentials_unavailable: t("pluginSettings.unableToReadCredentialConfigurationRepairThePluginDeclaration"),
    workspace_mcp_catalog_unavailable: t("pluginSettings.unableToValidateMcpDeclarationsCheckThePluginContract"),
    workspace_hook_catalog_unavailable: t("pluginSettings.unableToValidateHooksCheckThePluginDeclarationOr"),
    plugin_inspection_unavailable: t("pluginSettings.pluginDetailsAreTemporarilyUnavailablePleaseTryAgain"),
  };
  return plugin.errors.map((code) => messages[code] || code).join(" ");
}

function canEnable(plugin) {
  return plugin.errors.length === 0 && plugin.mcpServers !== null && plugin.hooks !== null;
}

function pluginStatusKey(plugin) {
  return plugin.errors.length ? "extensions.needsAttention" : plugin.enabled ? "pluginSettings.enabled" : "extensions.pluginDisabled";
}

export default function PluginSettings({ workspace, isSuperuser, filter = "", catalog }) {
  const { t } = useTranslation();
  const [plugins, setPlugins] = useState(null);
  const [pluginRevision, setPluginRevision] = useState(0);
  const [notice, setNotice] = useState("");
  const [removePlugin, setRemovePlugin] = useState(null);
  const uploadInputRef = useRef(null);
  const [credentials, setCredentials] = useState(null);
  const [credentialError, setCredentialError] = useState("");
  const [credentialRevision, setCredentialRevision] = useState(0);
  const [expandedPluginNames, setExpandedPluginNames] = useState([]);
  const [busyPlugin, setBusyPlugin] = useState("");
  const [busyCredential, setBusyCredential] = useState("");
  const [credentialDrafts, setCredentialDrafts] = useState({});
  const [deleteCredential, setDeleteCredential] = useState(null);
  const [error, setError] = useState("");
  const pluginRowsRef = useRef(null);
  const activeWorkspace = useRef(workspace?.id);
  activeWorkspace.current = workspace?.id;
  const canManage = Boolean(workspace && (isSuperuser || ["owner", "admin"].includes(workspace.role)));
  const apiBase = `/api/workspaces/${encodeURIComponent(workspace?.id || "")}`;
  const hasExpandedPlugins = expandedPluginNames.length > 0;

  useEffect(() => {
    if (!hasExpandedPlugins || deleteCredential) return undefined;
    function collapseOutside(event) {
      if (!pluginRowsRef.current?.contains(event.target)) setExpandedPluginNames([]);
    }
    document.addEventListener("pointerdown", collapseOutside, true);
    return () => document.removeEventListener("pointerdown", collapseOutside, true);
  }, [hasExpandedPlugins, deleteCredential]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Package mutations explicitly refresh this workspace catalog.
  useEffect(() => {
    let active = true;
    setPlugins(null);
    setBusyPlugin("");
    setError("");
    if (!workspace?.id) return () => { active = false; };
    apiJson(`/api/workspaces/${workspace.id}/plugins`)
      .then((result) => {
        if (!active) return;
        setPlugins(result.plugins);
        setExpandedPluginNames((current) => current.filter((name) => result.plugins.some((plugin) => plugin.name === name)));
        for (const plugin of result.plugins) {
          apiJson(`/api/workspaces/${workspace.id}/plugins/${plugin.name}`)
            .then(({ plugin: detail }) => {
              if (!active) return;
              setPlugins((items) => items.map((item) => item.name === detail.name && item.packageDigest === detail.packageDigest
                ? { ...detail, enabled: item.enabled } : item));
            })
            .catch(() => {
              if (!active) return;
              setPlugins((items) => items.map((item) => item.name === plugin.name
                ? { ...item, errors: [...item.errors, "plugin_inspection_unavailable"] } : item));
            });
        }
      })
      .catch((requestError) => active && setError(errorText(requestError)));
    return () => { active = false; };
  }, [workspace?.id, pluginRevision]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: The revision is the retry button's explicit credential refresh signal.
  useEffect(() => {
    let active = true;
    setCredentials(null);
    setCredentialError("");
    setCredentialDrafts({});
    if (!canManage) return () => { active = false; };
    apiJson(`${apiBase}/mcp-bearer-credentials`)
      .then((result) => active && setCredentials(result.credentials))
      .catch((requestError) => active && setCredentialError(errorText(requestError)));
    return () => { active = false; };
  }, [canManage, apiBase, credentialRevision]);

  async function setEnabled(plugin, enabled) {
    const workspaceId = workspace.id;
    setBusyPlugin(plugin.name);
    setError("");
    try {
      const result = await apiJson(
        `/api/workspaces/${workspace.id}/plugins/${plugin.name}`,
        jsonOptions("PATCH", { enabled }),
      );
      if (activeWorkspace.current !== workspaceId) return;
      setPlugins((items) => items?.map((item) => item.name === plugin.name
        ? enabled ? result.plugin : { ...item, enabled: false } : item));
    } catch (requestError) {
      if (activeWorkspace.current === workspaceId) setError(errorText(requestError));
    } finally {
      if (activeWorkspace.current === workspaceId) setBusyPlugin("");
    }
  }

  function updateCredentialDraft(key, patch) {
    setCredentialDrafts((current) => ({
      ...current,
      [key]: { secret: "", ...current[key], ...patch },
    }));
  }

  async function saveCredential(pluginName, credentialRef, existing) {
    if (!canManage) return;
    const workspaceId = workspace.id;
    const key = credentialKey(pluginName, credentialRef);
    const draft = credentialDrafts[key] || {};
    setBusyCredential(key);
    setCredentialError("");
    try {
      let result;
      if (existing && existing.scope !== "platform" && existing.writable !== false) {
        result = await apiJson(
          `${apiBase}/mcp-bearer-credentials/${existing.id}/rotate`,
          jsonOptions("POST", { secret: draft.secret || "" }),
        );
      } else {
        result = await apiJson(`${apiBase}/mcp-bearer-credentials`, jsonOptions("POST", {
          pluginName,
          credentialRef,
          displayName: `${pluginName} · ${credentialRef}`,
          secret: draft.secret || "",
        }));
      }
      if (activeWorkspace.current !== workspaceId) return;
      setCredentialDrafts((current) => ({ ...current, [key]: { secret: "" } }));
      setCredentials((items) => [...(items || []).filter((item) => item.id !== result.credential.id), result.credential]);
    } catch (requestError) {
      if (activeWorkspace.current === workspaceId) setCredentialError(errorText(requestError));
    } finally {
      if (activeWorkspace.current === workspaceId) setBusyCredential("");
    }
  }

  async function confirmDeleteCredential() {
    if (!deleteCredential || !canManage || deleteCredential.scope === "platform" || deleteCredential.writable === false) return;
    const workspaceId = workspace.id;
    const key = credentialKey(deleteCredential.pluginName, deleteCredential.credentialRef);
    setBusyCredential(key);
    setCredentialError("");
    try {
      await apiJson(`${apiBase}/mcp-bearer-credentials/${deleteCredential.id}`, { method: "DELETE" });
      if (activeWorkspace.current !== workspaceId) return;
      setDeleteCredential(null);
      setCredentials((items) => items.filter((item) => item.id !== deleteCredential.id));
    } catch (requestError) {
      if (activeWorkspace.current === workspaceId) setCredentialError(errorText(requestError));
    } finally {
      if (activeWorkspace.current === workspaceId) setBusyCredential("");
    }
  }

  async function upload(file) {
    if (!file || !canManage) return;
    const workspaceId = workspace.id;
    const body = new FormData();
    body.append("file", file);
    setBusyPlugin("upload"); setError(""); setNotice("");
    try {
      await apiJson(`${apiBase}/plugins/upload`, { method: "POST", body });
      if (activeWorkspace.current !== workspaceId) return;
      setPluginRevision(value => value + 1);
      setNotice(t("workspaceResources.packageUploaded"));
    } catch (requestError) {
      if (activeWorkspace.current === workspaceId) setError(errorText(requestError));
    } finally {
      if (activeWorkspace.current === workspaceId) {
        setBusyPlugin("");
        if (uploadInputRef.current) uploadInputRef.current.value = "";
      }
    }
  }
  async function uninstall() {
    if (!removePlugin || !canManage || removePlugin.scope !== "workspace" || removePlugin.writable === false) return;
    const workspaceId = workspace.id;
    setBusyPlugin(removePlugin.name); setError("");
    try {
      await apiJson(`${apiBase}/plugins/${encodeURIComponent(removePlugin.name)}/installation`, { method: "DELETE" });
      if (activeWorkspace.current !== workspaceId) return;
      setRemovePlugin(null); setPluginRevision(value => value + 1);
    } catch (requestError) {
      if (activeWorkspace.current === workspaceId) setError(errorText(requestError));
    } finally {
      if (activeWorkspace.current === workspaceId) setBusyPlugin("");
    }
  }

  if (!workspace) return <div className="capabilitySettingsEmpty">{t("pluginSettings.noWorkspaceAvailableToConfigure")}</div>;

  const selected = catalog?.selectedName ? plugins?.find(plugin => plugin.name === catalog.selectedName) : null;
  const visiblePlugins = catalog?.selectedName ? (selected ? [selected] : [])
    : filter
    ? plugins?.filter(plugin => `${plugin.displayName} ${plugin.shortDescription || ""} ${plugin.name}`.toLocaleLowerCase().includes(filter))
    : plugins;
  const uploadControl = canManage ? <>
    <input ref={uploadInputRef} type="file" accept=".zip,application/zip" aria-label={t("workspaceResources.choosePackage")} disabled={Boolean(busyPlugin)} onChange={event => upload(event.target.files?.[0])} />
    <button type="button" disabled={Boolean(busyPlugin)} onClick={() => uploadInputRef.current?.click()}><Upload aria-hidden="true" />{t("workspaceResources.uploadPackage")}</button>
  </> : null;
  const uploadHints = <>
    <small>{t("workspaceResources.skillPackageHint")}</small>
    <small>{t("workspaceResources.environmentVersionHint")}</small>
  </>;

  return (
    <div className="pluginSettings">
      {catalog ? <>
        {catalog.selectedName ? catalog.back : null}
        <header className="extensionHeading">
          <div><h1 tabIndex={catalog.selectedName ? -1 : undefined}>{selected?.displayName || t("settingsRoute.workspacePlugins")}</h1>{selected?.shortDescription ? <p>{selected.shortDescription}</p> : null}</div>
          {selected && canManage ? <div className="pluginSettingsToggle">
            <span>{t(busyPlugin === selected.name ? "libraryObjectRoute.saving" : selected.enabled ? "pluginSettings.enabled" : "extensions.pluginDisabled")}</span>
            <Switch checked={selected.enabled} disabled={Boolean(busyPlugin) || (!selected.enabled && !canEnable(selected))}
              aria-label={`${selected.enabled ? t("pluginSettings.disable") : t("pluginSettings.enable")} ${selected.displayName}`}
              onCheckedChange={enabled => void setEnabled(selected, enabled)} />
          </div> : !catalog.selectedName && uploadControl ? <div className="globalPluginUpload extensionUpload">{uploadControl}</div> : null}
        </header>
        {selected ? <div className="pluginSettingsMeta">
          <span className={selected.errors.length ? "managementAttention" : undefined}>{t(pluginStatusKey(selected))}</span>
          {selected.version ? <span>{t("globalPluginSettings.version")} {selected.version}</span> : null}
          <span>{t(selected.scope === "platform" ? "workspaceResources.sharedReadOnly" : "settingsRoute.workspace")}</span>
        </div> : null}
        {!catalog.selectedName ? <div className="extensionToolbar">
          {catalog.search}
          {canManage ? <details className="extensionUploadHelp"><summary>{t("extensions.packageHelp")}</summary>{uploadHints}</details> : null}
        </div> : null}
      </> : uploadControl ? <div className="globalPluginUpload pluginPackageUpload">{uploadControl}{uploadHints}</div> : null}
      {notice ? <p className="globalPluginNotice" role="status">{notice}</p> : null}
      {error ? <div className="capabilitySettingsError" role="alert">{t("pluginSettings.unableToLoadPlugin")}{error}</div> : null}
      {canManage && credentialError ? <div className="capabilitySettingsError" role="alert">{t("pluginSettings.credentialActionFailed")}{credentialError} <button className="pluginEnableButton" type="button" disabled={Boolean(busyCredential)} onClick={() => setCredentialRevision((value) => value + 1)}>{t("pluginSettings.reloadCredentials")}</button></div> : null}
      {plugins === null && !error ? <div className="capabilitySettingsEmpty">{t("pluginSettings.loadingPlugins")}</div> : null}
      {plugins?.length === 0 && !error && !catalog?.selectedName ? <div className="capabilitySettingsEmpty">{t("pluginSettings.thisDistributionHasNoPlugins")}</div> : null}
      {plugins && catalog?.selectedName && !selected ? <div className="capabilitySettingsEmpty" role="status">{t("extensions.pluginNotFound")}</div> : null}
      {plugins?.length > 0 && visiblePlugins.length === 0 && !catalog?.selectedName ? <div className="capabilitySettingsEmpty" role="status">{t("extensions.noMatches")}</div> : null}
      <div className={catalog && !catalog.selectedName && visiblePlugins?.length ? "settingsPanel settingsListPanel pluginSettingsRows" : "pluginSettingsRows"} ref={pluginRowsRef} role="list" aria-label={t("pluginSettings.availablePlugins")}>
        {(visiblePlugins || []).map((plugin) => {
          const expanded = Boolean(catalog?.selectedName) || expandedPluginNames.includes(plugin.name);
          const detailId = `plugin-detail-${plugin.name}`;
          const credentialRefs = expanded ? [...new Set([
            ...(plugin.mcpCredentialRefs || []),
            ...(credentials || []).filter((item) => item.pluginName === plugin.name).map((item) => item.credentialRef),
          ])] : [];
          return <article className="pluginSettingsEntry" role="listitem" key={plugin.name}>
            {!catalog?.selectedName ? <button className="managementRow managementRowButton pluginSettingsIdentity" type="button" id={`${detailId}-toggle`}
              aria-expanded={catalog ? undefined : expanded} aria-controls={catalog ? undefined : detailId}
              onClick={() => catalog ? catalog.onSelect(plugin.name) : setExpandedPluginNames(current => current.includes(plugin.name) ? [] : [plugin.name])}
              aria-label={t("pluginSettings.viewDetailsForValue", { value1: plugin.displayName })}>
              <span className="managementRowIcon"><Blocks aria-hidden="true" /></span>
              <span className="managementRowCopy"><strong>{plugin.displayName}</strong><small>{plugin.shortDescription || plugin.name}</small></span>
              <span className="managementRowEnd">
                <span className={plugin.errors.length ? "managementAttention" : undefined}>{t(pluginStatusKey(plugin))}</span>
                <ChevronRight aria-hidden="true" />
              </span>
            </button> : null}
            <div className={`pluginSettingsDetail${catalog ? " isPage" : ""}`} id={detailId} role="region" aria-label={catalog ? plugin.displayName : undefined} aria-labelledby={catalog ? undefined : `${detailId}-toggle`} hidden={!expanded}>
              {expanded ? <>
                {!catalog ? <div className="managementToolbar">
                  <span>{t(plugin.errors.length ? "extensions.needsAttention" : canEnable(plugin) ? "pluginSettings.declarationsValidated" : "pluginSettings.checkingDeclarations")}</span>
                  {canManage ? <button
                    className={plugin.enabled ? "pluginEnableButton is-enabled" : "pluginEnableButton"}
                    type="button"
                    disabled={busyPlugin === plugin.name || (!plugin.enabled && !canEnable(plugin))}
                    aria-label={`${plugin.enabled ? t("pluginSettings.disable") : t("pluginSettings.enable")} ${plugin.displayName}`}
                    aria-pressed={plugin.enabled}
                    onClick={() => void setEnabled(plugin, !plugin.enabled)}
                  >{busyPlugin === plugin.name ? t("libraryObjectRoute.saving") : plugin.enabled ? t("pluginSettings.enabled") : t("pluginSettings.enable")}</button> : null}
                </div> : null}
                {plugin.errors.length ? <p className="capabilitySettingsError" role="alert">{pluginErrors(plugin)}{" "}{t("pluginSettings.otherPluginsAreUnaffectedYouCanStillDisableThis")}</p> : null}
                {canManage && plugin.scope === "workspace" && plugin.writable !== false ? <div className="pluginSettingsActions"><button className="pluginEnableButton is-danger" type="button" disabled={Boolean(busyPlugin)} onClick={() => setRemovePlugin(plugin)}>{t("globalPluginSettings.remove")}</button></div> : null}

                <section className="settingsPanel pluginSettingsSection" aria-labelledby={`${detailId}-general-heading`}>
                  <h3 id={`${detailId}-general-heading`}>{t("pluginSettings.general")}</h3>
                  {!catalog && plugin.scope === "platform" ? <p className="pluginSettingsMuted">{t("workspaceResources.sharedReadOnly")}</p> : null}
                  <div className="pluginSettingsProperty">
                    <strong>{t("pluginSettings.capabilities")}</strong>
                    <p>{plugin.capabilities.length ? plugin.capabilities.join("、") : t("pluginSettings.notDeclared")}</p>
                  </div>
                </section>

                <section className="settingsPanel pluginSettingsSection" aria-labelledby={`${detailId}-connections-heading`}>
                  <h3 id={`${detailId}-connections-heading`}>{t("pluginSettings.connection")}</h3>
                  {plugin.mcpServers === null ? <p className="pluginSettingsMuted">{t("pluginSettings.mcpConnectionDetailsHaveNotBeenValidatedAConnection")}</p> : plugin.mcpServers.length ? <div className="pluginConnectionRows">{plugin.mcpServers.map((server) => <div className="pluginConnectionRow" key={server.id}>
                    <span><strong>{server.id}</strong><small>{server.transport.type === "streamableHttp" ? t("pluginSettings.networkService") : t("pluginSettings.localService")}</small></span>
                    <em>{server.auth.type === "none" ? t("pluginSettings.noConfigurationNeeded") : canManage && credentials === null ? t("pluginSettings.credentialStatusUnknown") : (canManage ? credentials.some((item) => item.pluginName === plugin.name && item.credentialRef === server.auth.credentialRef) : server.auth.credentialConfigured) ? t("pluginSettings.credentialsSaved") : t("pluginSettings.credentialsRequired")}</em>
                  </div>)}</div> : <p className="pluginSettingsMuted">{t("pluginSettings.thisPluginNeedsNoAdditionalConnection")}</p>}
                  {canManage && credentialRefs.length ? <div className="pluginCredentialSection">
                    {credentialRefs.map((credentialRef) => {
                      const key = credentialKey(plugin.name, credentialRef);
                      const matches = credentials?.filter(credential => credential.pluginName === plugin.name && credential.credentialRef === credentialRef) || [];
                      const existing = matches.find(credential => credential.scope !== "platform" && credential.writable !== false) || matches[0];
                      const draft = credentialDrafts[key] || {};
                      return <form className="pluginCredentialForm" key={credentialRef} onSubmit={(event) => {
                        event.preventDefault();
                        void saveCredential(plugin.name, credentialRef, existing);
                      }}>
                        <div><strong>{credentialRef}</strong>{existing?.scope === "platform" ? <small>{t("workspaceResources.sharedReadOnly")}</small> : null}<small>{existing ? t("pluginSettings.configuredVValue", { value1: existing.version }) : t("modelSettings.notConfiguredYet")}</small></div>
                        <input aria-label={`${credentialRef} Bearer Token`} type="password" autoComplete="new-password" placeholder={existing ? t("pluginSettings.newTokenOrBearer") : t("pluginSettings.tokenOrBearer")} value={draft.secret || ""} onChange={(event) => updateCredentialDraft(key, { secret: event.target.value })} />
                        <button type="submit" disabled={credentials === null || busyCredential === key || !draft.secret}>{busyCredential === key ? t("libraryObjectRoute.saving") : existing && existing.scope !== "platform" && existing.writable !== false ? t("pluginSettings.rotate") : t("modelSettings.save")}</button>
                        {existing && existing.scope !== "platform" && existing.writable !== false ? <button type="button" className="is-danger" disabled={busyCredential === key} onClick={() => setDeleteCredential(existing)}>{t("appRoute.delete")}</button> : null}
                      </form>;
                    })}
                  </div> : null}
                </section>

                <details className="pluginDeveloperDetails">
                  <summary>{t("pluginSettings.developerInformation")}</summary>
                  <dl className="pluginDeveloperFacts">
                    <div><dt>{t("interface.package")}</dt><dd>{plugin.name}</dd></div>
                    <div><dt>{t("globalPluginSettings.version")}</dt><dd>{plugin.version}</dd></div>
                    <div><dt>SHA-256</dt><dd>{plugin.packageDigest}</dd></div>
                  </dl>
                  {plugin.hooks === null ? <p>{t("pluginSettings.hooksHaveNotBeenValidated")}</p> : plugin.hooks.length ? <section className="pluginMcpSection">
                    <header><h3>{t("interface.lifecycleHooks")}</h3></header>
                    <div className="pluginMcpServers">{plugin.hooks.map((hook) => <article className="pluginMcpServer" key={hook.id}>
                      <header><strong>{hook.id}</strong><span>{hook.event}</span><em>{hook.timeoutMs} ms</em></header>
                      {hook.matcher ? <code>{hook.matcher}</code> : null}
                    </article>)}</div>
                  </section> : null}
                  {plugin.mcpServers?.length ? <section className="pluginMcpSection">
                    <header><h3>{t("interface.mcpServers")}</h3></header>
                    <div className="pluginMcpServers">{plugin.mcpServers.map((server) => <article className="pluginMcpServer" key={server.id}>
                      <header><strong>{server.id}</strong><span>{server.transport.type === "streamableHttp" ? "Streamable HTTP" : "stdio"}</span><em>{server.auth.type === "none" ? t("pluginSettings.noAuthentication") : server.auth.credentialRef}</em></header>
                      {server.transport.endpoint ? <code>{server.transport.endpoint}</code> : null}
                      <div className="pluginMcpTools" aria-label={t("pluginSettings.valueDeclaredTools", { value1: server.id })}>{server.tools.map((tool) => <div key={tool.sourceName}>
                        <span><strong>{tool.name}</strong><small>{tool.sourceName}</small></span>
                      </div>)}</div>
                    </article>)}</div>
                  </section> : null}
                </details>
              </> : null}
            </div>
          </article>;
        })}
      </div>
      <ConfirmDialog open={Boolean(removePlugin)} title={t("workspaceResources.removePackage")} message={t("workspaceResources.removePackageHint")} confirmLabel={t("globalPluginSettings.remove")} onCancel={() => setRemovePlugin(null)} onConfirm={() => void uninstall()} busy={Boolean(busyPlugin)} />
      <ConfirmDialog
        open={Boolean(deleteCredential)}
        title={t("pluginSettings.deleteBearerCredential")}
        message={deleteCredential ? t("pluginSettings.afterDeletingValueNewRunsCannotConnectToThe", { value1: deleteCredential.displayName }) : ""}
        confirmLabel={t("appRoute.delete")}
        busy={Boolean(deleteCredential && busyCredential === credentialKey(deleteCredential.pluginName, deleteCredential.credentialRef))}
        onCancel={() => setDeleteCredential(null)}
        onConfirm={() => void confirmDeleteCredential()}
      />
    </div>
  );
}
