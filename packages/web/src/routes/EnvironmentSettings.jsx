import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";

function readPolicy(value) {
  if (!value || Object.keys(value).length !== 1 || !["disabled", "publicInternet"].includes(value.network)) {
    throw new Error("environment_settings_invalid");
  }
  return value.network;
}

export default function EnvironmentSettings({ workspace }) {
  const { t } = useTranslation();
  const [network, setNetwork] = useState(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const requestRef = useRef(null);
  const url = `/api/workspaces/${encodeURIComponent(workspace.id)}/environment-settings`;
  const load = useCallback(async () => {
    requestRef.current?.abort();
    const controller = new AbortController(); requestRef.current = controller;
    setNetwork(null); setBusy(true); setError(""); setSaved(false);
    try {
      const value = readPolicy(await apiJson(url, { signal: controller.signal }));
      if (!controller.signal.aborted) { setNetwork(value); return true; }
    } catch {
      if (!controller.signal.aborted) setError("loadError");
    } finally { if (!controller.signal.aborted) setBusy(false); }
    return false;
  }, [url]);
  useEffect(() => { void load(); return () => requestRef.current?.abort(); }, [load]);

  async function update(event) {
    if (busy || network === null) return;
    const requested = event.target.checked ? "publicInternet" : "disabled";
    const controller = new AbortController(); requestRef.current?.abort(); requestRef.current = controller;
    setBusy(true); setError(""); setSaved(false);
    try {
      const value = readPolicy(await apiJson(url, { ...jsonOptions("PATCH", { network: requested }), signal: controller.signal }));
      if (!controller.signal.aborted) { setNetwork(value); setSaved(true); }
    } catch {
      if (!controller.signal.aborted && await load()) setError("saveUnconfirmed");
    } finally { if (!controller.signal.aborted) setBusy(false); }
  }

  return <div className="preferenceSettings">
    <section>
      <h1>{t("environmentSettings.title")}</h1>
      <label className="preferenceRow">
        <span><strong>{t("environmentSettings.internet")}</strong><small>{t("environmentSettings.newEnvironmentsOnly")}</small></span>
        <input type="checkbox" role="switch" aria-label={t("environmentSettings.internet")} checked={network === "publicInternet"} disabled={busy || network === null} onChange={update} />
      </label>
      {busy && network === null ? <p role="status">{t("environmentSettings.loading")}</p> : null}
      {error ? <p role="alert">{t(`environmentSettings.${error}`)}</p> : null}
      {error === "loadError" ? <button type="button" onClick={load} disabled={busy}>{t("environmentSettings.retry")}</button> : null}
      {saved ? <p role="status">{t("environmentSettings.saved")}</p> : null}
    </section>
  </div>;
}
