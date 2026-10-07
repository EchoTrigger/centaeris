import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Boxes, Upload } from "lucide-react";
import { ApiError, apiJson } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";


const ERROR_MESSAGES = () => ({
  plugin_already_installed: t("globalPluginSettings.thisPluginIsAlreadyInstalled"),
  plugin_archive_too_large: t("globalPluginSettings.thisZipExceedsThePluginInstallationLimit"),
  plugin_credentials_configured: t("globalPluginSettings.removeThisPluginSMcpCredentialsFirst"),
  plugin_enabled_in_workspaces: t("globalPluginSettings.disableThisPluginInEveryWorkspaceFirst"),
  plugin_in_active_agent_runs: t("globalPluginSettings.runningOrQueuedJobsStillUseThisPlugin"),
  plugin_lifecycle_unavailable: t("globalPluginSettings.thePluginCatalogOrRuntimeIsTemporarilyUnavailable"),
  plugin_lifecycle_request_invalid: t("globalPluginSettings.thePluginRequestContainsUnsupportedFields"),
  plugin_archive_invalid: t("globalPluginSettings.thisZipIsNotAValidPluginPackage"),
  plugin_package_layout_invalid: t("globalPluginSettings.theZipMustContainOneCompletePluginDirectory"),
  plugin_package_invalid: t("globalPluginSettings.pluginManifestOrResourceValidationFailed"),
  plugin_not_installed: t("globalPluginSettings.thisPluginIsNotInstalled"),
});


function errorText(error) {
  return error instanceof ApiError
    ? ERROR_MESSAGES()[error.message] || t("globalPluginSettings.unableToCompleteThePluginAction")
    : error instanceof Error ? error.message : String(error);
}


function removalBlocker(plugin) {
  if (plugin.enabledWorkspaceCount) return t("globalPluginSettings.stillUsedByValueWorkspaces", { value1: plugin.enabledWorkspaceCount });
  if (plugin.credentialCount) return t("globalPluginSettings.valueCredentialsAreStillSaved", { value1: plugin.credentialCount });
  if (!plugin.removable) return t("globalPluginSettings.inUseByRunningJobs");
  return "";
}


export default function GlobalPluginSettings() {
  const { t } = useTranslation();
  const [plugins, setPlugins] = useState(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [removeTarget, setRemoveTarget] = useState(null);
  const uploadInputRef = useRef(null);

  useEffect(() => {
    let active = true;
    apiJson("/api/admin/plugins")
      .then((result) => active && setPlugins(result.plugins))
      .catch((requestError) => active && setError(errorText(requestError)));
    return () => { active = false; };
  }, []);

  async function remove(plugin) {
    const key = `${plugin.name}:remove`;
    setBusy(key);
    setError("");
    setNotice("");
    try {
      const result = await apiJson(
        `/api/admin/plugins/${plugin.name}`,
        { method: "DELETE" },
      );
      void result;
      setPlugins((items) => items.filter((item) => item.name !== plugin.name));
      setNotice(t("globalPluginSettings.valueWasRemoved", { value1: plugin.displayName }));
      setRemoveTarget(null);
    } catch (requestError) {
      setError(errorText(requestError));
    } finally {
      setBusy("");
    }
  }

  async function upload(file) {
    if (!file) return;
    setBusy("upload");
    setError("");
    setNotice("");
    const body = new FormData();
    body.append("file", file);
    try {
      const result = await apiJson("/api/admin/plugins/upload", { method: "POST", body });
      const wasInstalled = plugins?.some((item) => item.name === result.plugin.name) ?? false;
      setPlugins((items) => {
        const current = items || [];
        const existing = current.findIndex((item) => item.name === result.plugin.name);
        if (existing < 0) return [...current, result.plugin];
        return current.map((item, index) => index === existing ? result.plugin : item);
      });
      setNotice(t(wasInstalled ? "plugins.updated" : "plugins.installed", { name: result.plugin.displayName }));
    } catch (requestError) {
      setError(errorText(requestError));
    } finally {
      setBusy("");
      if (uploadInputRef.current) uploadInputRef.current.value = "";
    }
  }

  return <div className="globalPluginSettings">
    <div className="globalPluginUpload">
      <input
        ref={uploadInputRef}
        type="file"
        accept=".zip,application/zip"
        aria-label={t("globalPluginSettings.choosePluginZip")}
        onChange={(event) => void upload(event.target.files?.[0])}
      />
      <button type="button" disabled={Boolean(busy) || plugins === null} onClick={() => uploadInputRef.current?.click()}>
        <Upload aria-hidden="true" />
        {busy === "upload" ? t("globalPluginSettings.installing") : t("globalPluginSettings.uploadZip")}
      </button>
    </div>
    {error ? <p className="capabilitySettingsError" role="alert">{error}</p> : null}
    {notice ? <p className="globalPluginNotice" role="status" aria-live="polite">{notice}</p> : null}
    {plugins === null && !error ? <div className="capabilitySettingsEmpty" role="status">{t("globalPluginSettings.loadingGlobalPlugins")}</div> : null}
    {plugins?.length === 0 ? <div className="capabilitySettingsEmpty">{t("globalPluginSettings.noPluginsInstalledYet")}</div> : null}
    {plugins?.length ? <div className="globalPluginRows" role="list" aria-label={t("globalPluginSettings.globalPlugins")}>
      {plugins.map((plugin) => {
        const blocker = removalBlocker(plugin);
        const actionBusy = busy.startsWith(`${plugin.name}:`);
        return <article className="globalPluginEntry" role="listitem" aria-busy={actionBusy} key={plugin.name}>
          <div className="globalPluginIdentity">
            <Boxes aria-hidden="true" />
            <span>
              <strong>{plugin.displayName}</strong>
              <code translate="no">{plugin.name}</code>
            </span>
          </div>
          <p>{plugin.shortDescription || t("globalPluginSettings.noDescriptionProvided")}</p>
          <div className="globalPluginFacts">
            <span className="isInstalled">{t("globalPluginSettings.installed")}</span>
            <small>{t("globalPluginSettings.version")}{" "}<b>{plugin.version}</b></small>
            {plugin.enabledWorkspaceCount ? <small>{t("plugins.workspaces", { count: plugin.enabledWorkspaceCount })}</small> : null}
            {plugin.credentialCount ? <small>{t("plugins.credentials", { count: plugin.credentialCount })}</small> : null}
            {plugin.errors?.includes("plugin_manifest_invalid") ? <small className="isError">{t("globalPluginSettings.unableToReadManifest")}</small> : null}
          </div>
          <div className="globalPluginActions">
            <button className="isDanger" type="button" disabled={actionBusy || Boolean(blocker)} title={blocker || undefined} aria-label={t("attachmentCard.removeValue", { value1: plugin.displayName })} onClick={() => setRemoveTarget(plugin)}>{t("globalPluginSettings.remove")}</button>
          </div>
        </article>;
      })}
    </div> : null}
    <ConfirmDialog
      open={Boolean(removeTarget)}
      title={t("globalPluginSettings.removeGlobalPlugin")}
      message={removeTarget ? t("globalPluginSettings.removingValuePreventsAllWorkspacesFromEnablingItUpload", { value1: removeTarget.displayName }) : ""}
      confirmLabel={t("globalPluginSettings.remove")}
      busy={Boolean(removeTarget && busy === `${removeTarget.name}:remove`)}
      onCancel={() => setRemoveTarget(null)}
      onConfirm={() => void remove(removeTarget)}
    />
  </div>;
}
