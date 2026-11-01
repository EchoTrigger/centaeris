import { useEffect, useRef, useState } from "react";
import { apiJson, jsonOptions } from "../api";
import { useTranslation } from "../i18n";

export function SessionInstructions({ session, onSaved }) {
  const { t } = useTranslation();
  const [instructions, setInstructions] = useState(session.instructions);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const request = useRef(null);
  useEffect(() => () => request.current?.abort(), []);

  async function save(event) {
    event.preventDefault();
    if (busy) return;
    const controller = new AbortController(); request.current = controller;
    setBusy(true); setError("");
    try {
      const result = await apiJson(`/api/sessions/${encodeURIComponent(session.id)}`, {
        ...jsonOptions("PATCH", { instructions }), signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      if (result.session?.id !== session.id || result.session.workspaceId !== session.workspaceId) throw new Error("session_response_invalid");
      onSaved(result.session);
    } catch {
      if (!controller.signal.aborted) setError(t("sessionInstructions.saveFailed"));
    } finally { if (!controller.signal.aborted) setBusy(false); }
  }

  return <details className="sessionInstructions">
    <summary>{t("sessionInstructions.title")}</summary>
    <form onSubmit={save}>
      <label>{t("sessionInstructions.title")}<textarea value={instructions} onChange={event => setInstructions(event.target.value)} disabled={busy} /></label>
      <small>{t("sessionInstructions.hint")}</small>
      {error ? <p role="alert">{error}</p> : null}
      <button type="submit" disabled={busy || instructions === session.instructions}>{t(busy ? "modelSettings.saving" : "modelSettings.save")}</button>
    </form>
  </details>;
}
