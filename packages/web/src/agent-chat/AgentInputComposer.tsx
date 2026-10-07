import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ArrowUp, LoaderCircle, Plus, RotateCcw } from "lucide-react";
import { ApiError, apiJson } from "../api";
import { useTranslation } from "../i18n";
import { createAgentModelSettingsClient, type AgentModelSettings } from "./agentModelSettings";
import { createAgentInputClient } from "./agentInputClient";
import { createAgentInputDraftStore } from "./agentInputDraft";
import { prepareAgentInputSubmission, type PreparedAgentInputBinding, type AgentInputAttachment } from "./preparedAgentInput";
import { mergeAgentAttachments, uploadAgentAttachments } from "./agentAttachments";
import { AgentInputAttachments } from "./AgentInputAttachments";
import "./agent-attachments.css";

export function AgentInputComposer({ agentId, workspaceId, userId, sessionId, onAccepted, onAuthorityLost }: Readonly<{
  agentId: string; workspaceId: string; userId: string; sessionId?: string;
  onAccepted: () => void | Promise<void>; onAuthorityLost: () => void;
}>) {
  const { t } = useTranslation();
  const client = useMemo(() => createAgentInputClient({ userId, workspaceId, agentId }, apiJson), [userId, workspaceId, agentId]);
  const draftStore = useMemo(() => createAgentInputDraftStore({ userId, workspaceId, agentId }), [userId, workspaceId, agentId]);
  const modelClient = useMemo(() => createAgentModelSettingsClient(agentId, apiJson), [agentId]);
  const [draftState, setDraftState] = useState(() => draftStore.read());
  const draft = draftState.body;
  const attachments = draftState.attachments;
  const draftRef = useRef(draftState); draftRef.current = draftState;
  const [uploading, setUploading] = useState(false);
  const [attachmentError, setAttachmentError] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const uploadBusyRef = useRef(false);
  const mountedRef = useRef(true);
  const [pending, setPending] = useState(false);
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState(false);
  const [model, setModel] = useState<AgentModelSettings | null>(null);
  const [error, setError] = useState<"load" | "uncertain" | "conflict" | "binding" | "text" | null>(null);
  const busyRef = useRef(false);
  const pendingIdRef = useRef<string | undefined>(undefined);
  const requestRef = useRef<AbortController | null>(null);
  const propsRef = useRef({ sessionId, onAuthorityLost }); propsRef.current = { sessionId, onAuthorityLost };
  const loadBinding = useCallback(async (signal: AbortSignal, allowCreate = false): Promise<PreparedAgentInputBinding | null> => {
    let binding;
    try { binding = await client.loadBinding(signal); }
    catch (error) {
      if (!(error instanceof ApiError && error.status === 404 && error.message === "coordination_session_not_found")) throw error;
      if (client.pending() || propsRef.current.sessionId) throw new Error("agent_input_binding_invalid");
      if (!allowCreate) return null;
      binding = await client.createBinding(signal);
    }
    if ((propsRef.current.sessionId && propsRef.current.sessionId !== binding.sessionId)
      || (client.pending() && client.pending()?.binding.sessionId !== binding.sessionId)) throw new Error("agent_input_binding_invalid");
    return binding;
  }, [client]);
  const load = useCallback(async () => {
    requestRef.current?.abort(); const controller = new AbortController(); requestRef.current = controller;
    setReady(false); setError(null);
    try {
      const stored = client.pending(); pendingIdRef.current = stored?.submission.inputId;
      setPending(Boolean(stored)); if (stored) {
        const storedDraft = draftStore.read();
        const restored = stored.submission.attachmentRefs.map(inputRef => storedDraft.attachments.find(item => item.inputRef === inputRef) ?? { inputRef, displayName: inputRef, contentType: "application/octet-stream" });
        setDraftState({ body: stored.submission.body, attachments: restored, persisted: storedDraft.persisted });
      }
      const [settings] = await Promise.all([modelClient.load(controller.signal), loadBinding(controller.signal)]);
      if (!controller.signal.aborted) { setModel(settings); setReady(true); }
    } catch (error) {
      if (!controller.signal.aborted) {
        const authorityLost = error instanceof Error && error.message === "agent_input_binding_invalid"
          || error instanceof ApiError && [401, 403, 404].includes(error.status);
        setError(authorityLost ? "binding" : "load"); if (authorityLost) propsRef.current.onAuthorityLost();
      }
    }
  }, [client, modelClient, loadBinding, draftStore]);
  useEffect(() => { void load(); return () => { requestRef.current?.abort(); }; }, [load]);
  useEffect(() => { mountedRef.current = true; return () => { mountedRef.current = false; }; }, []);
  const saveDraft = (body: string, items: readonly AgentInputAttachment[]) => {
    const next = { body, attachments: items, persisted: draftStore.write(body, items) };
    draftRef.current = next; if (mountedRef.current) setDraftState(next);
  };
  const addAttachments = async (files: readonly File[]) => {
    if (uploadBusyRef.current || busyRef.current || pending || !ready || error === "binding" || error === "conflict") return;
    if (!files.length || files.length + draftRef.current.attachments.length > 50) { setAttachmentError(true); return; }
    uploadBusyRef.current = true; setUploading(true); setAttachmentError(false);
    try {
      const binding = await loadBinding(new AbortController().signal, true);
      if (!binding) throw new Error("agent_input_binding_invalid");
      const incoming = await uploadAgentAttachments(binding, files, apiJson);
      const current = draftRef.current;
      saveDraft(current.body, mergeAgentAttachments(current.attachments, incoming));
    } catch (failure) {
      if (mountedRef.current) {
        setAttachmentError(true);
        if (failure instanceof ApiError && [401, 403, 404].includes(failure.status)) { setReady(false); setError("binding"); propsRef.current.onAuthorityLost(); }
      }
    } finally { uploadBusyRef.current = false; if (mountedRef.current) setUploading(false); }
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busyRef.current || uploadBusyRef.current || !ready || model?.status !== "configured" || error === "conflict") return;
    const refs = attachments.map(item => item.inputRef);
    try { prepareAgentInputSubmission("validate", draft, refs); } catch { setError("text"); return; }
    busyRef.current = true; setBusy(true); setError(null);
    requestRef.current?.abort(); const controller = new AbortController(); requestRef.current = controller;
    try {
      const settings = await modelClient.load(controller.signal);
      if (controller.signal.aborted) return;
      setModel(settings);
      if (settings.status !== "configured") return;
      const binding = await loadBinding(controller.signal, true);
      if (!binding || controller.signal.aborted) return;
      await client.submit(binding, draft, controller.signal, pendingIdRef.current, refs);
      // A verified acceptance consumes the draft even if navigation has already
      // aborted this component's display update.
      const persisted = draftStore.write("", []);
      if (controller.signal.aborted) return;
      pendingIdRef.current = undefined; setPending(false); setDraftState({ body: "", attachments: [], persisted });
      // HTTP acceptance is complete before history refresh; display failures
      // cannot turn an accepted input back into an uncertain submission.
      try { await onAccepted(); } catch { /* The history route owns its error. */ }
    } catch (error) {
      if (controller.signal.aborted) return;
      let stored: ReturnType<typeof client.pending> = null;
      try { stored = client.pending(); }
      catch { setReady(false); setError("load"); return; }
      pendingIdRef.current ??= stored?.submission.inputId;
      setPending(Boolean(pendingIdRef.current));
      if (error instanceof ApiError && error.status === 409) setError("conflict");
      else if (error instanceof Error && error.message === "agent_input_binding_invalid"
        || error instanceof ApiError && [401, 403, 404].includes(error.status)) {
        setReady(false); setError("binding"); propsRef.current.onAuthorityLost();
      } else setError("uncertain");
    } finally { busyRef.current = false; if (!controller.signal.aborted) setBusy(false); }
  };
  let validText = true;
  try { prepareAgentInputSubmission("validate", draft, attachments.map(item => item.inputRef)); } catch { validText = false; }
  const statusKey = ready && model?.status === "unconfigured" ? "agentChat.modelRequired" : ready && model?.status === "unavailable" ? "agentChat.modelUnavailable" : null;
  const descriptionId = `agent-input:${[userId, workspaceId, agentId].map(encodeURIComponent).join(":")}`;
  const descriptions = [statusKey ? `${descriptionId}:status` : null, error ? `${descriptionId}:error` : null, !draftState.persisted ? `${descriptionId}:draft` : null].filter(Boolean).join(" ") || undefined;
  return <><form className="workspaceComposer agentInputComposer" onSubmit={event => void submit(event)} onDragOver={event => { if (event.dataTransfer.types.includes("Files")) event.preventDefault(); }} onDrop={event => { if (event.dataTransfer.files.length) { event.preventDefault(); void addAttachments(Array.from(event.dataTransfer.files)); } }}>
    {attachments.length ? <AgentInputAttachments attachments={attachments} agentId={agentId} onRemove={!pending && !busy && !uploading ? inputRef => saveDraft(draft, attachments.filter(item => item.inputRef !== inputRef)) : undefined} /> : null}
    <textarea name="agentMessage" autoComplete="off" aria-label={t("agentChat.inputLabel")} aria-describedby={descriptions} placeholder={t("agentChat.inputPlaceholder")} value={draft} disabled={busy || pending} rows={2} onChange={event => saveDraft(event.target.value, attachments)} onPaste={event => { const files = Array.from(event.clipboardData.files); if (files.length && !event.clipboardData.getData("text/plain")) { event.preventDefault(); void addAttachments(files); } }} />
    {attachmentError ? <p role="alert">{t("agentChat.attachmentUploadError")}</p> : null}
    {error ? <p id={`${descriptionId}:error`} role="alert">{t(error === "conflict" ? "agentChat.inputConflict" : error === "binding" ? "agentChat.inputBindingLost" : error === "text" ? "agentChat.inputInvalid" : error === "load" ? "agentChat.inputLoadError" : "agentChat.inputUncertain")}</p> : null}
    {!draftState.persisted ? <p id={`${descriptionId}:draft`} role="alert">{t("agentChat.draftSaveError")}</p> : null}
    {error === "load" || error === "binding" ? <button type="button" disabled={busy} onClick={() => void load()}>{t("agentChat.retry")}</button> : null}
    <div className="workspaceComposerFooter agentInputComposerActions">
      <div className="workspaceComposerControlGroup">
        <button className="workspaceComposerIconButton" type="button" aria-label={t("appRoute.add")} title={t("appRoute.add")} disabled={!ready || busy || pending || uploading} onClick={() => fileInputRef.current?.click()}>{uploading ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : <Plus aria-hidden="true" />}</button>
      </div>
      <input ref={fileInputRef} className="srOnly" type="file" multiple aria-label={t("appRoute.selectOneOrMoreMaterials")} disabled={!ready || busy || pending || uploading} onChange={event => { const files = Array.from(event.currentTarget.files ?? []); event.currentTarget.value = ""; void addAttachments(files); }} />
      {statusKey ? <p className="agentInputComposerStatus" id={`${descriptionId}:status`} role="status">{t(statusKey)}</p> : null}
      <button className="workspaceSendButton" type="submit" aria-label={t(busy ? "agentChat.sending" : pending ? "agentChat.retrySend" : "agentChat.send")} title={t(busy ? "agentChat.sending" : pending ? "agentChat.retrySend" : "agentChat.send")} disabled={!ready || busy || uploading || !validText || model?.status !== "configured" || error === "conflict"}>{busy ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : pending ? <RotateCcw aria-hidden="true" /> : <ArrowUp aria-hidden="true" />}</button>
    </div>
  </form></>;
}
