import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { ApiError, apiJson, apiUrl } from "../api";
import { useTranslation } from "../i18n";
import { DocumentPreview } from "../components/DocumentPreview";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import { officeFileType } from "../chat/officeFormats.mjs";
import { parseMessageFiles, parseSessionPreview, sessionPreviewPath, type FilePreviewFact, type MessageFileBinding, type SessionPreviewPage } from "./agentPreviewData";
import type { PreviewFile } from "./agentChatViewTypes";
import type { ReferencedSession } from "./agentOutputFeed";
import { AgentSessionReference } from "./AgentSessionReference";
import { FileOutput } from "lucide-react";

const revoked = (error: unknown) => error instanceof ApiError && [401, 403, 404, 409, 410].includes(error.status);
function usePreview(sessionId: string, revision = "") {
  const [page, setPage] = useState<SessionPreviewPage | null>(null);
  const pageRef = useRef(page); pageRef.current = page;
  const [error, setError] = useState(false);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [authorized, setAuthorized] = useState(false);
  const controllerRef = useRef<AbortController | null>(null);
  const loadedPagesRef = useRef(1);
  const loadedSessionRef = useRef<string | null>(null);
  const revisionRef = useRef(revision);
  const latestRevisionRef = useRef(revision); latestRevisionRef.current = revision;
  const load = useCallback(async (after: string | null = null, quiet = false) => {
    controllerRef.current?.abort(); const controller = new AbortController(); controllerRef.current = controller;
    setRefreshing(true); setLoading(!quiet || !pageRef.current); setError(false);
    try {
      let value = parseSessionPreview(await apiJson<unknown>(sessionPreviewPath(sessionId, after), { signal: controller.signal }), sessionId, after);
      let loadedPages = 1;
      // Refresh the authoritative window the reader opened. Keeping cached rows
      // without revalidating each page would retain withdrawn output metadata.
      while (after === null && loadedPages < loadedPagesRef.current && value.hasMore && !controller.signal.aborted) {
        const cursor = value.nextAfterArtifactId;
        const nextPage = parseSessionPreview(await apiJson<unknown>(sessionPreviewPath(sessionId, cursor), { signal: controller.signal }), sessionId, cursor);
        if (value.outputs.some(row => nextPage.outputs.some(next => next.objectRef === row.objectRef))) throw new Error("agent_preview_output_identity_invalid");
        value = { ...nextPage, outputs: [...value.outputs, ...nextPage.outputs] };
        loadedPages++;
      }
      if (!controller.signal.aborted) {
        const previous = pageRef.current;
        if (after !== null && (!previous || previous.nextAfterArtifactId !== after || previous.outputs.some(row => value.outputs.some(next => next.objectRef === row.objectRef)))) throw new Error("agent_preview_output_identity_invalid");
        const next = after !== null && previous ? { ...value, outputs: [...previous.outputs, ...value.outputs] } : value;
        loadedPagesRef.current = after !== null ? loadedPagesRef.current + 1 : loadedPages;
        pageRef.current = next; setPage(next); setAuthorized(true);
      }
    } catch (error) { if (!controller.signal.aborted) { setError(true); if (revoked(error)) { pageRef.current = null; setPage(null); setAuthorized(false); } } }
    finally { if (!controller.signal.aborted) { setLoading(false); setRefreshing(false); } }
  }, [sessionId]);
  useLayoutEffect(() => {
    if (loadedSessionRef.current !== sessionId) { loadedSessionRef.current = sessionId; loadedPagesRef.current = 1; pageRef.current = null; setPage(null); }
    // Activity keeps the opened window while suspending Effects. Hide cached
    // names before revealing that DOM, then reauthorize every opened page.
    setAuthorized(false); revisionRef.current = latestRevisionRef.current; void load();
    return () => { controllerRef.current?.abort(); setAuthorized(false); };
  }, [load, sessionId]);
  useEffect(() => { if (!refreshing && revisionRef.current !== revision) { revisionRef.current = revision; void load(null, true); } }, [revision, load, refreshing]);
  const invalidate = useCallback(() => { controllerRef.current?.abort(); pageRef.current = null; setPage(null); setAuthorized(false); setLoading(false); setError(true); }, []);
  return { page, error, loading, authorized, load, invalidate };
}
export function AgentSessionOutputs({ sessionId, onPreviewFile }: Readonly<{ sessionId: string; onPreviewFile: PreviewFile }>) {
  const { t } = useTranslation(); const { page, loading, error, authorized, load, invalidate } = usePreview(sessionId);
  return <div>
    <button type="button" disabled={loading} onClick={() => void load()}>{t("agentChat.refresh")}</button>
    {error ? <p role="alert">{t("agentChat.outputsError")} <button type="button" onClick={() => void load()}>{t("agentChat.retry")}</button></p> : null}
    <div className="agentPreviewFiles" aria-hidden={!authorized} inert={!authorized}>{page?.outputs.map(file => <button className="agentFileReference" type="button" key={file.objectRef} onClick={() => onPreviewFile({ file, invalidateMetadata: invalidate }, sessionId)}>{file.displayName}</button>)}</div>
    {!loading && !error && !page?.outputs.length ? <p>{t("agentChat.noOutputs")}</p> : null}
    {page?.hasMore ? <button type="button" disabled={loading} onClick={() => void load(page.nextAfterArtifactId)}>{t("agentChat.loadMoreOutputs")}</button> : null}
  </div>;
}
function OverviewSession({ session, revision, onPreviewFile, onRunState }: Readonly<{
  session: ReferencedSession; onPreviewFile: PreviewFile;
  revision?: string;
  onRunState: (id: string, state: "running" | "unknown") => void;
}>) {
  const { t } = useTranslation();
  const { page, error, loading, authorized, load, invalidate } = usePreview(session.sessionId, revision);
  const runState = authorized && !error && page?.runFact?.status === "running" ? "running" : "unknown";
  useEffect(() => { onRunState(session.sessionId, runState); }, [session.sessionId, runState, onRunState]);
  return <div className="agentChatOverviewSession">
    <div className="agentPreviewFiles" aria-hidden={!authorized} inert={!authorized}>
      {page?.outputs.map(file => <button className="agentFileReference" type="button" key={file.objectRef} onClick={() => onPreviewFile({ file, invalidateMetadata: invalidate }, session.sessionId)}>{file.displayName}</button>)}
    </div>
    {error ? <p role="alert">{t("agentChat.previewError")} <button type="button" onClick={() => void load()}>{t("agentChat.retry")}</button></p> : null}
    {page?.hasMore ? <button className="agentChatShowMore" type="button" onClick={() => void load(page.nextAfterArtifactId)} disabled={loading}>{t("agentChat.showMore")}</button> : null}
  </div>;
}
export function AgentOverviewSessions(props: Readonly<{
  sessions: readonly ReferencedSession[]; onPreviewSession: (id: string) => void; onPreviewFile: PreviewFile;
  revision?: string;
  onRunState: (id: string, state: "running" | "unknown") => void;
}>) {
  const { t } = useTranslation();
  return <><section className="agentChatOverviewActivity" aria-label={t("agentChat.activity")}>
    <h2 className="agentChatOverviewSectionTitle">{t("agentChat.activity")}</h2>
    {props.sessions.map(session => <AgentSessionReference key={session.sessionId} session={session} onPreview={props.onPreviewSession} />)}
  </section><section className="agentChatOverviewOutputs" aria-label={t("agentChat.outputs")}>
    <h2 className="agentChatOverviewSectionTitle">{t("agentChat.outputs")}</h2>
    {props.sessions.map(session => <OverviewSession key={session.sessionId} {...props} session={session} />)}
  </section></>;
}
export function AgentMessageFiles({ messageId, binding, onPreviewFile }: Readonly<{ messageId: string; binding: MessageFileBinding; onPreviewFile: PreviewFile }>) {
  const { t } = useTranslation(); const [files, setFiles] = useState<readonly FilePreviewFact[]>([]); const [error, setError] = useState(false);
  const controllerRef = useRef<AbortController | null>(null);
  const refs = JSON.stringify(binding.inputRefs); const { agentId, sessionId, agentRunId } = binding;
  const load = useCallback(async () => {
    controllerRef.current?.abort(); const controller = new AbortController(); controllerRef.current = controller;
    setFiles([]); setError(false);
    try {
      const value = await apiJson<unknown>(`/api/agents/${encodeURIComponent(agentId)}/messages/${encodeURIComponent(messageId)}/files`, { signal: controller.signal });
      const parsed = parseMessageFiles(value, messageId, { agentId, sessionId, agentRunId, inputRefs: JSON.parse(refs) });
      if (!controller.signal.aborted) setFiles(parsed);
    } catch { if (!controller.signal.aborted) setError(true); }
  }, [agentId, sessionId, agentRunId, messageId, refs]);
  useEffect(() => { void load(); return () => { controllerRef.current?.abort(); }; }, [load]);
  const invalidate = useCallback(() => { controllerRef.current?.abort(); setFiles([]); setError(true); }, []);
  return <div className="agentMessageFiles">{files.map(file => <button className="agentFileReference" type="button" key={file.inputRef} aria-label={file.displayName} title={file.displayName} onClick={() => onPreviewFile({ file, invalidateMetadata: invalidate }, sessionId)}><FileOutput aria-hidden="true" /><span>{file.displayName}</span></button>)}
    {error ? <p role="alert">{t("agentChat.filesError")} <button type="button" onClick={() => void load()}>{t("agentChat.retry")}</button></p> : null}</div>;
}
// The Agent boundary consumes image/PDF bytes itself so an actual HTTP denial
// reaches its metadata owner. Ordinary Session embeds keep their existing path.
function AgentBinaryPreview({ file, onError }: Readonly<{ file: FilePreviewFact; onError: (error: unknown) => void }>) {
  const { t } = useTranslation(); const [state, setState] = useState({ src: "", url: "", error: false });
  const src = apiUrl(file.previewUrl); const image = file.contentType.startsWith("image/");
  useEffect(() => {
    const controller = new AbortController(); let objectUrl = "";
    void fetch(src, { credentials: "include", cache: "no-store", signal: controller.signal }).then(async response => {
      if (!response.ok) throw Object.assign(new Error("preview_unavailable"), { status: response.status });
      const contentType = response.headers.get("Content-Type")?.split(";", 1)[0].trim().toLowerCase();
      if (image ? !contentType?.startsWith("image/") : contentType !== "application/pdf") throw new Error("preview_unavailable");
      const bytes = await response.blob();
      if (controller.signal.aborted) return;
      objectUrl = URL.createObjectURL(bytes); setState({ src, url: objectUrl, error: false });
    }).catch(error => { if (!controller.signal.aborted) { setState({ src, url: "", error: true }); onError(error); } });
    return () => { controller.abort(); if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [src, image, onError]);
  if (state.src !== src) return null;
  if (state.error) return <p role="alert">{t("agentChat.filesError")}</p>;
  return image ? <img src={state.url} alt={file.displayName} /> : <iframe src={state.url} title={file.displayName} />;
}
export function AgentFilePreview({ file, onUnavailable }: Readonly<{ file: FilePreviewFact; onUnavailable?: () => void }>) {
  const { t } = useTranslation(); const [revision, setRevision] = useState(0);
  const onError = useCallback((error: unknown) => {
    if (error && typeof error === "object" && "status" in error && typeof error.status === "number" && [401, 403, 404, 409, 410].includes(error.status)) onUnavailable?.();
  }, [onUnavailable]);
  const image = file.contentType.startsWith("image/");
  const supported = image || Boolean(officeFileType(file.displayName)) || file.contentType === "application/pdf" || file.contentType.startsWith("text/") || codePreviewCanRender(file.displayName, file.contentType);
  return <div className="agentFilePreview">
    <p><a href={apiUrl(file.downloadUrl)}>{t("agentChat.downloadFile")}</a>{supported ? <button type="button" onClick={() => setRevision(value => value+1)}>{t("agentChat.retryPreview")}</button> : null}</p>
    {image || file.contentType === "application/pdf" ? <AgentBinaryPreview key={revision} file={file} onError={onError} /> : supported ? <DocumentPreview key={revision} src={apiUrl(file.previewUrl)} title={file.displayName} contentType={file.contentType} onError={onError} /> : <p>{t("agentChat.filePreviewUnsupported")}</p>}
  </div>;
}
