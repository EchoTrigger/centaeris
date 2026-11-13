import { useCallback, useEffect, useId, useRef, useState } from "react";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";
import "./environmentSettings.css";

const RESOURCE_FIELDS = [
  { key: "cpuMilli", label: "cpu", scale: 1000, maximum: 4294967295, step: "0.001" },
  { key: "memoryBytes", label: "memory", scale: 2 ** 30, maximum: Number.MAX_SAFE_INTEGER, step: "any" },
  { key: "workspaceBytes", label: "workspaceSpace", scale: 2 ** 30, maximum: Number.MAX_SAFE_INTEGER, step: "any" },
];
const POLICY_KEYS = ["network", "cpuMilli", "memoryBytes", "workspaceBytes", "revision"];

function readPolicy(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== POLICY_KEYS.length
    || !POLICY_KEYS.every(key => Object.hasOwn(value, key))
    || !["disabled", "publicInternet"].includes(value.network)
    || !Number.isInteger(value.revision) || value.revision <= 0
    || !RESOURCE_FIELDS.every(field => Number.isSafeInteger(value[field.key]) && value[field.key] > 0 && value[field.key] <= field.maximum)) {
    throw new Error("environment_settings_invalid");
  }
  return value;
}

function draftFromPolicy(policy) {
  return Object.fromEntries([
    ["network", policy.network],
    ...RESOURCE_FIELDS.map(field => [field.key, String(policy[field.key] / field.scale)]),
  ]);
}

function initialState(url) {
  return { url, policy: null, draft: { network: "disabled", cpuMilli: "", memoryBytes: "", workspaceBytes: "" }, busy: true, error: "", saved: false, invalid: [] };
}

function resourceValue(value, field) {
  const units = Number(value);
  const amount = Math.round(units * field.scale);
  // Division must round-trip: e.g. 1.001 cores is valid despite multiplication
  // rounding, while fractions that cannot represent whole milliCPU/bytes are not.
  return value.trim() !== "" && Number.isFinite(units) && Number.isSafeInteger(amount)
    && amount > 0 && amount <= field.maximum && amount / field.scale === units ? amount : null;
}

export default function EnvironmentSettings({ workspace }) {
  const { t } = useTranslation();
  const url = `/api/workspaces/${encodeURIComponent(workspace.id)}/environment-settings`;
  const [state, setState] = useState(() => initialState(url));
  const requestRef = useRef(null);
  const formId = useId();
  const current = state.url === url ? state : initialState(url);
  const { policy, draft, busy, error, saved, invalid } = current;
  const disabled = busy || policy === null;

  const load = useCallback(async () => {
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    setState(initialState(url));
    try {
      const value = readPolicy(await apiJson(url, { signal: controller.signal }));
      if (!controller.signal.aborted && requestRef.current === controller) {
        setState({ ...initialState(url), policy: value, draft: draftFromPolicy(value), busy: false });
      }
    } catch {
      if (!controller.signal.aborted && requestRef.current === controller) {
        setState(previous => ({ ...previous, busy: false, error: "loadError" }));
      }
    }
  }, [url]);
  useEffect(() => { void load(); return () => requestRef.current?.abort(); }, [load]);

  function change(key, value) {
    if (disabled) return;
    setState(previous => ({ ...previous, draft: { ...previous.draft, [key]: value }, saved: false, error: "", invalid: previous.invalid.filter(field => field !== key) }));
  }

  async function save(event) {
    event.preventDefault();
    if (disabled) return;
    const payload = { network: draft.network };
    const invalidFields = [];
    for (const field of RESOURCE_FIELDS) {
      const amount = resourceValue(draft[field.key], field);
      if (amount === null) invalidFields.push(field.key);
      else payload[field.key] = amount;
    }
    if (invalidFields.length) {
      setState(previous => ({ ...previous, invalid: invalidFields, saved: false, error: "" }));
      return;
    }
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    setState(previous => ({ ...previous, busy: true, error: "", saved: false, invalid: [] }));
    try {
      const value = readPolicy(await apiJson(url, { ...jsonOptions("PATCH", payload), signal: controller.signal }));
      if (!controller.signal.aborted && requestRef.current === controller) {
        setState(previous => ({ ...previous, policy: value, draft: draftFromPolicy(value), saved: true }));
      }
    } catch {
      if (controller.signal.aborted || requestRef.current !== controller) return;
      try {
        const value = readPolicy(await apiJson(url, { signal: controller.signal }));
        if (!controller.signal.aborted && requestRef.current === controller) {
          setState(previous => ({ ...previous, policy: value, draft: draftFromPolicy(value), error: "saveUnconfirmed" }));
        }
      } catch {
        if (!controller.signal.aborted && requestRef.current === controller) {
          setState(previous => ({ ...previous, policy: null, error: "loadError" }));
        }
      }
    } finally {
      if (!controller.signal.aborted && requestRef.current === controller) setState(previous => ({ ...previous, busy: false }));
    }
  }

  return <div className="preferenceSettings">
    <header className="settingsPageHeading"><h1>{t("environmentSettings.title")}</h1></header>
    <section className="settingsPanel environmentSettings">
      <form className="environmentSettingsForm" onSubmit={save} noValidate>
        <label className="preferenceRow">
          <span><strong>{t("environmentSettings.internet")}</strong></span>
          <input type="checkbox" role="switch" aria-label={t("environmentSettings.internet")} checked={draft.network === "publicInternet"} disabled={disabled} onChange={event => change("network", event.target.checked ? "publicInternet" : "disabled")} />
        </label>
        {RESOURCE_FIELDS.map(field => {
          const id = `${formId}-${field.key}`;
          const isInvalid = invalid.includes(field.key);
          return <div className="environmentSettingsResource" key={field.key}>
            <label htmlFor={id}>{t(`environmentSettings.${field.label}`)}</label>
            <div>
              <input id={id} type="number" aria-label={t(`environmentSettings.${field.label}`)} aria-invalid={isInvalid} aria-describedby={isInvalid ? `${id}-error` : undefined} min={1 / field.scale} max={field.maximum / field.scale} step={field.step} value={draft[field.key]} disabled={disabled} onChange={event => change(field.key, event.target.value)} />
              {isInvalid ? <small id={`${id}-error`} role="alert">{t("environmentSettings.invalidResource")}</small> : null}
            </div>
          </div>;
        })}
        <button className="secondary" type="submit" disabled={disabled}>{t("environmentSettings.save")}</button>
      </form>
      {error ? <p role="alert">{t(`environmentSettings.${error}`)}</p> : null}
      {error === "loadError" ? <button className="secondary" type="button" onClick={load} disabled={busy}>{t("environmentSettings.retry")}</button> : null}
      {saved ? <p role="status">{t("environmentSettings.saved")}</p> : null}
    </section>
  </div>;
}
