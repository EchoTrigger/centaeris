import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Blocks, ChevronRight } from "lucide-react";
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

export default function PluginSettings({ workspace, isSuperuser }) {
  const { t } = useTranslation();
  const [plugins, setPlugins] = useState(null);
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
  const canManage = ["owner", "admin"].includes(workspace?.role);
  const hasExpandedPlugins = expandedPluginNames.length > 0;

  useEffect(() => {
    if (!hasExpandedPlugins || deleteCredential) return undefined;
    function collapseOutside(event) {
      if (!pluginRowsRef.current?.contains(event.target)) setExpandedPluginNames([]);
    }
    document.addEventListener("pointerdown", collapseOutside, true);
    return () => document.removeEventListener("pointerdown", collapseOutside, true);
  }, [hasExpandedPlugins, deleteCredential]);

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
  }, [workspace?.id]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: The revision is the retry button's explicit credential refresh signal.
  useEffect(() => {
    let active = true;
    setCredentials(null);
    setCredentialError("");
    setCredentialDrafts({});
    if (!isSuperuser) return () => { active = false; };
    apiJson("/api/admin/mcp-bearer-credentials")
      .then((result) => active && setCredentials(result.credentials))
      .catch((requestError) => active && setCredentialError(errorText(requestError)));
    return () => { active = false; };
  }, [isSuperuser, credentialRevision]);

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
    const key = credentialKey(pluginName, credentialRef);
    const draft = credentialDrafts[key] || {};
    setBusyCredential(key);
    setCredentialError("");
    try {
      let result;
      if (existing) {
        result = await apiJson(
          `/api/admin/mcp-bearer-credentials/${existing.id}/rotate`,
          jsonOptions("POST", { secret: draft.secret || "" }),
        );
      } else {
        result = await apiJson("/api/admin/mcp-bearer-credentials", jsonOptions("POST", {
          pluginName,
          credentialRef,
          displayName: `${pluginName} · ${credentialRef}`,
          secret: draft.secret || "",
        }));
      }
      setCredentialDrafts((current) => ({ ...current, [key]: { secret: "" } }));
      setCredentials((items) => [...(items || []).filter((item) => item.id !== result.credential.id), result.credential]);
    } catch (requestError) {
      setCredentialError(errorText(requestError));
    } finally {
      setBusyCredential("");
    }
  }

  async function confirmDeleteCredential() {
    if (!deleteCredential) return;
    const key = credentialKey(deleteCredential.pluginName, deleteCredential.credentialRef);
    setBusyCredential(key);
    setCredentialError("");
    try {
      await apiJson(`/api/admin/mcp-bearer-credentials/${deleteCredential.id}`, { method: "DELETE" });
      setDeleteCredential(null);
      setCredentials((items) => items.filter((item) => item.id !== deleteCredential.id));
    } catch (requestError) {
      setCredentialError(errorText(requestError));
    } finally {
      setBusyCredential("");
    }
  }

  if (!workspace) return <div className="capabilitySettingsEmpty">{t("pluginSettings.noWorkspaceAvailableToConfigure")}</div>;

  return (
    <div className="pluginSettings">
      {error ? <div className="capabilitySettingsError" role="alert">{t("pluginSettings.unableToLoadPlugin")}{error}</div> : null}
      {isSuperuser && credentialError ? <div className="capabilitySettingsError" role="alert">{t("pluginSettings.credentialActionFailed")}{credentialError} <button className="pluginEnableButton" type="button" disabled={Boolean(busyCredential)} onClick={() => setCredentialRevision((value) => value + 1)}>{t("pluginSettings.reloadCredentials")}</button></div> : null}
      {plugins === null && !error ? <div className="capabilitySettingsEmpty">{t("pluginSettings.loadingPlugins")}</div> : null}
      {plugins?.length === 0 && !error ? <div className="capabilitySettingsEmpty">{t("pluginSettings.thisDistributionHasNoPlugins")}</div> : null}
      <div className="pluginSettingsRows" ref={pluginRowsRef} role="list" aria-label={t("pluginSettings.availablePlugins")}>
        {(plugins || []).map((plugin) => {
          const expanded = expandedPluginNames.includes(plugin.name);
          const detailId = `plugin-detail-${plugin.name}`;
          const credentialRefs = expanded ? [...new Set([
            ...(plugin.mcpCredentialRefs || []),
            ...(credentials || []).filter((item) => item.pluginName === plugin.name).map((item) => item.credentialRef),
          ])] : [];
          return <article className="pluginSettingsEntry" role="listitem" key={plugin.name}>
            <div className="pluginSettingsRow">
              <button className="pluginSettingsIdentity" type="button" id={`${detailId}-toggle`} aria-expanded={expanded} aria-controls={detailId} onClick={() => setExpandedPluginNames((current) => current.includes(plugin.name) ? current.filter((name) => name !== plugin.name) : [...current, plugin.name])} aria-label={t("pluginSettings.viewDetailsForValue", { value1: plugin.displayName })}>
                <Blocks aria-hidden="true" />
                <span><strong>{plugin.displayName}</strong><small>{plugin.shortDescription || plugin.name}</small><small role={plugin.errors.length ? "status" : undefined}>{plugin.errors.length ? pluginErrors(plugin) : canEnable(plugin) ? t("pluginSettings.declarationsValidated") : t("pluginSettings.checkingDeclarations")}</small></span>
                <ChevronRight aria-hidden="true" />
              </button>
              <button
                className={plugin.enabled ? "pluginEnableButton is-enabled" : "pluginEnableButton"}
                type="button"
                disabled={!canManage || busyPlugin === plugin.name || (!plugin.enabled && !canEnable(plugin))}
                aria-label={`${plugin.enabled ? t("pluginSettings.disable") : t("pluginSettings.enable")} ${plugin.displayName}`}
                aria-pressed={plugin.enabled}
                onClick={() => void setEnabled(plugin, !plugin.enabled)}
              >{busyPlugin === plugin.name ? t("libraryObjectRoute.saving") : plugin.enabled ? t("pluginSettings.enabled") : t("pluginSettings.enable")}</button>
            </div>
            <div className="pluginSettingsDetail" id={detailId} role="region" aria-labelledby={`${detailId}-toggle`} hidden={!expanded}>
              {expanded ? <>
                {plugin.errors.length ? <p className="capabilitySettingsError" role="alert">{pluginErrors(plugin)}{" "}{t("pluginSettings.otherPluginsAreUnaffectedYouCanStillDisableThis")}</p> : null}

                <section className="pluginSettingsSection" aria-labelledby={`${detailId}-general-heading`}>
                  <h3 id={`${detailId}-general-heading`}>{t("pluginSettings.general")}</h3>
                  <div className="pluginSettingsProperty">
                    <strong>{t("pluginSettings.capabilities")}</strong>
                    <p>{plugin.capabilities.length ? plugin.capabilities.join("、") : t("pluginSettings.notDeclared")}</p>
                  </div>
                </section>

                <section className="pluginSettingsSection" aria-labelledby={`${detailId}-connections-heading`}>
                  <h3 id={`${detailId}-connections-heading`}>{t("pluginSettings.connection")}</h3>
                  {plugin.mcpServers === null ? <p className="pluginSettingsMuted">{t("pluginSettings.mcpConnectionDetailsHaveNotBeenValidatedAConnection")}</p> : plugin.mcpServers.length ? <div className="pluginConnectionRows">{plugin.mcpServers.map((server) => <div className="pluginConnectionRow" key={server.id}>
                    <span><strong>{server.id}</strong><small>{server.transport.type === "streamableHttp" ? t("pluginSettings.networkService") : t("pluginSettings.localService")}</small></span>
                    <em>{server.auth.type === "none" ? t("pluginSettings.noConfigurationNeeded") : isSuperuser && credentials === null ? t("pluginSettings.credentialStatusUnknown") : (isSuperuser ? credentials.some((item) => item.pluginName === plugin.name && item.credentialRef === server.auth.credentialRef) : server.auth.credentialConfigured) ? t("pluginSettings.credentialsSaved") : t("pluginSettings.credentialsRequired")}</em>
                  </div>)}</div> : <p className="pluginSettingsMuted">{t("pluginSettings.thisPluginNeedsNoAdditionalConnection")}</p>}
                  {isSuperuser && credentialRefs.length ? <div className="pluginCredentialSection">
                    {credentialRefs.map((credentialRef) => {
                      const key = credentialKey(plugin.name, credentialRef);
                      const existing = credentials?.find((credential) => credential.pluginName === plugin.name && credential.credentialRef === credentialRef);
                      const draft = credentialDrafts[key] || {};
                      return <form className="pluginCredentialForm" key={credentialRef} onSubmit={(event) => {
                        event.preventDefault();
                        void saveCredential(plugin.name, credentialRef, existing);
                      }}>
                        <div><strong>{credentialRef}</strong><small>{existing ? t("pluginSettings.configuredVValue", { value1: existing.version }) : t("modelSettings.notConfiguredYet")}</small></div>
                        <input aria-label={`${credentialRef} Bearer Token`} type="password" autoComplete="new-password" placeholder={existing ? t("pluginSettings.newTokenOrBearer") : t("pluginSettings.tokenOrBearer")} value={draft.secret || ""} onChange={(event) => updateCredentialDraft(key, { secret: event.target.value })} />
                        <button type="submit" disabled={credentials === null || busyCredential === key || !draft.secret}>{busyCredential === key ? t("libraryObjectRoute.saving") : existing ? t("pluginSettings.rotate") : t("modelSettings.save")}</button>
                        {existing ? <button type="button" className="is-danger" disabled={busyCredential === key} onClick={() => setDeleteCredential(existing)}>{t("appRoute.delete")}</button> : null}
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
