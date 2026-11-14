import { useEffect, useState } from "react";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";

export function ModelDefaults({ workspaceId, models }) {
  const { t } = useTranslation();
  const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}/model-defaults`;
  const [defaults, setDefaults] = useState(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState(null);
  const [revision, setRevision] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Revision reloads acknowledged settings on explicit retry.
  useEffect(() => {
    const controller = new AbortController();
    setDefaults(null); setError(null);
    apiJson(endpoint, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setDefaults(result.defaults);
    }).catch(failure => { if (!controller.signal.aborted) setError(failure.message); });
    return () => controller.abort();
  }, [endpoint, revision]);
  const available = models.filter(model => model.enabled && (!model.thinkingModes?.length || model.thinkingModes.includes("low")));
  async function save(value) {
    const model = available.find(item => item.id === value);
    setPending(true); setError(null);
    try {
      const result = await apiJson(endpoint, jsonOptions("PATCH", {
        titleModelConfigRef: value || null,
        titleThinkingMode: model && !model.thinkingModes?.length ? "" : "low",
      }));
      setDefaults(result.defaults);
    } catch (failure) { setError(failure.message); }
    finally { setPending(false); }
  }
  return <section className="settingsPanel modelDefaults" aria-busy={pending}>
    <label htmlFor="workspace-model-defaults">{t("modelSettings.defaultPurpose")}</label>
    <select id="workspace-model-defaults" value={defaults?.titleModelConfigRef || ""} disabled={!defaults || pending} onChange={event => void save(event.target.value)}>
      <option value="">{t("modelSettings.noDefaultPurpose")}</option>
      {defaults?.titleModelConfigRef && !available.some(model => model.id === defaults.titleModelConfigRef) ? <option value={defaults.titleModelConfigRef} disabled>{t("modelSettings.unavailableDefault")}</option> : null}
      {available.map(model => <option key={model.id} value={model.id}>{model.displayName || model.modelName}</option>)}
    </select>
    {error ? <div role="alert"><span>{error}</span><button type="button" disabled={pending} onClick={() => setRevision(value => value + 1)}>{t("agentChat.retry")}</button></div> : null}
  </section>;
}
