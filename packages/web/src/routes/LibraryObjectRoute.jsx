
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { useBlocker, useLocation, useNavigate, useParams, useRouteLoaderData, useSearchParams } from "react-router";
import { ArrowLeft, Download, FileText, LoaderCircle, LockKeyhole, Trash2 } from "lucide-react";
import { apiJson as api, apiUrl, jsonOptions } from "../api";
import { ShellPage } from "../shell/ShellPage";
import { DocumentPreview } from "../components/DocumentPreview";
import { officeFileType } from "../chat/officeFormats.mjs";
import { officePreviewUrl } from "../chat/attachments.mjs";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import SpreadsheetPreview from "../components/SpreadsheetPreview";
import { createNoteDraftStore } from "../library/noteDraftStore";
import { NoteSaveController } from "../library/noteSaveController";
import { ConfirmDialog } from "../components/ConfirmDialog";

function isImage(item) {
  return item.objectKind === "image" || item.contentType.startsWith("image/");
}

function canEmbed(item) {
  return Boolean(officeFileType(item.displayName)) || item.contentType === "application/pdf" || item.contentType.startsWith("text/") || codePreviewCanRender(item.displayName, item.contentType);
}

function isMarkdownFile(item) {
  const contentType = String(item?.contentType || "").split(";", 1)[0].trim().toLowerCase();
  return contentType === "text/markdown" || /\.(md|markdown)$/i.test(item?.displayName || "");
}

function LibraryPreviewPageContent() {
  const { t } = useTranslation();
  const params = useParams();
  const location = useLocation();
  const navigate = useNavigate();
  const { workspace } = useRouteLoaderData("workspace");
  const { user } = useRouteLoaderData("authenticated");
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const [searchParams] = useSearchParams();
  const libraryObjectId = String(params.libraryObjectId || "");
  const folderId = searchParams.get("folder") || "";
  const [item, setItem] = useState(null);
  const [markdown, setMarkdown] = useState("");
  const [noteTitle, setNoteTitle] = useState("");
  const [renamingTitle, setRenamingTitle] = useState(false);
  const [loading, setLoading] = useState(true);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState("");
  const [saveState, setSaveState] = useState(null);
  const [drafts, setDrafts] = useState([]);
  const [comparison, setComparison] = useState(null);
  const controllerRef = useRef(null);
  const confirmedNoteRef = useRef(null);
  const loadEpochRef = useRef(0);
  const storeRef = useRef(null);
  const fallbackDraftIdRef = useRef(crypto.randomUUID().replaceAll("-", ""));
  const clientDraftId = searchParams.get("draft") || fallbackDraftIdRef.current;
  const saveStatus = saveState ? t(`libraryObjectRoute.saveState.${saveState.status}`) : "";
  const blocker = useBlocker(({ currentLocation, nextLocation }) =>
    (currentLocation.pathname !== nextLocation.pathname || currentLocation.search !== nextLocation.search)
      && Boolean(controllerRef.current?.requiresDraftPersistence));

  useEffect(() => {
    if (blocker.state !== "blocked") return undefined;
    let cancelled = false;
    const controller = controllerRef.current;
    void controller?.persist().then(() => {
      if (!cancelled && controllerRef.current === controller && !controller.requiresDraftPersistence) blocker.proceed();
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [blocker]);

  function draftStore() {
    storeRef.current ||= createNoteDraftStore();
    return storeRef.current;
  }

  function beginController(note, document, epoch, recovered) {
    const previousSave = controllerRef.current?.dispose();
    let controller;
    controller = new NoteSaveController({
      userId: String(user.id), noteId: note.id || null, clientDraftId,
      editInstanceId: crypto.randomUUID(), parentFolderId: folderId,
      baseGeneration: note.contentGeneration || 0, document, recovered, store: draftStore(),
      send: async operation => {
        // A recovery controller shares the editor's single in-flight request boundary.
        await previousSave;
        return operation.noteId
          ? api(`/api/library/${encodeURIComponent(operation.noteId)}/note`, jsonOptions("PUT", {
            saveId: operation.saveId, baseGeneration: operation.baseGeneration, ...operation.document,
          }))
          : api("/api/library/notes", jsonOptions("POST", {
            saveId: operation.saveId, parentFolderId: operation.parentFolderId, ...operation.document,
          }));
      },
      onState: state => {
        if (epoch !== loadEpochRef.current || controllerRef.current !== controller) return;
        setSaveState(state);
        setError(state.error && state.status !== "conflict" ? t("libraryObjectRoute.unableToSaveValue", { value1: state.error }) : "");
      },
      onConfirmed: (result, confirmedDocument) => {
        if (epoch !== loadEpochRef.current || controllerRef.current !== controller) return;
        confirmedNoteRef.current = { note: result.object, document: confirmedDocument };
        setItem(result.object);
        if (!note.id && controller.state.status === "saved") {
          navigate(`${base}/library/${encodeURIComponent(result.object.id)}${folderId ? `?folder=${encodeURIComponent(folderId)}` : ""}`, { replace: true, state: null });
        }
      },
    });
    controllerRef.current = controller;
    setSaveState(controller.state);
    if (!note.id && document.markdown.trim()) void controller.persist().catch(() => {});
  }

  function backToLibrary() {
    navigate(folderId ? `${base}/library?folder=${encodeURIComponent(folderId)}` : `${base}/library`);
  }

  async function load(epoch = ++loadEpochRef.current) {
    confirmedNoteRef.current = null;
    setLoading(true);
    setItem(null);
    setNoteTitle("");
    setMarkdown("");
    setError("");
    setSaveState(null);
    setDrafts([]);
    setComparison(null);
    try {
      let note;
      let body = "";
      if (libraryObjectId === "new") {
        const draft = location.state?.noteDraft || { name: "Untitled", markdown: "" };
        note = { id: "", objectKind: "note", contentType: "text/markdown", displayName: draft.name || "Untitled", contentGeneration: 0, status: "draft" };
        body = draft.markdown || "";
        if (!searchParams.get("draft")) {
          navigate(`${base}/library/new?${new URLSearchParams({ draft: clientDraftId, ...(folderId ? { folder: folderId } : {}) })}`, { replace: true, state: location.state });
        }
      } else {
        const result = await api(`/api/library/${encodeURIComponent(libraryObjectId)}`);
        if (epoch !== loadEpochRef.current) return;
        note = result.object;
        if (note.id !== libraryObjectId) throw new Error("note_response_identity_mismatch");
        if (note.objectKind === "folder") {
          navigate(`${base}/library?folder=${encodeURIComponent(note.id)}`, { replace: true });
          return;
        }
        if (note.objectKind === "note") {
          const opened = await api(`/api/library/${encodeURIComponent(libraryObjectId)}/note`);
          if (epoch !== loadEpochRef.current) return;
          note = opened.object;
          if (note.id !== libraryObjectId || !Number.isSafeInteger(note.contentGeneration) || note.contentGeneration < 1) throw new Error("note_response_identity_mismatch");
          body = opened.markdown;
        }
      }
      if (epoch !== loadEpochRef.current) return;
      setItem(note);
      if (note.objectKind === "note") {
        const document = { displayName: note.displayName, markdown: body };
        confirmedNoteRef.current = { note, document };
        setNoteTitle(note.displayName === "Untitled" ? "" : note.displayName);
        setMarkdown(body);
        setRenamingTitle(!note.id);
        beginController(note, document, epoch);
        const local = await draftStore().list(String(user.id), note.id || clientDraftId);
        if (epoch === loadEpochRef.current) setDrafts(local);
      }
    } catch (requestError) {
      if (epoch === loadEpochRef.current) setError(t("libraryObjectRoute.unableToOpenFileValue", { value1: requestError.message }));
    } finally {
      if (epoch === loadEpochRef.current) setLoading(false);
    }
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: Scalar owner and route identities are the load boundary; render-local handlers must not retrigger it.
  useEffect(() => {
    const epoch = ++loadEpochRef.current;
    controllerRef.current?.dispose();
    controllerRef.current = null;
    void load(epoch);
    return () => {
      if (loadEpochRef.current === epoch) loadEpochRef.current++;
      controllerRef.current?.dispose();
      controllerRef.current = null;
    };
  }, [libraryObjectId, folderId, clientDraftId, user.id, workspace.id]);

  useEffect(() => {
    const persist = () => { void controllerRef.current?.persist().catch(() => {}); };
    const retry = () => { if (controllerRef.current?.state.status === "error") void controllerRef.current.flush(); };
    const beforeUnload = event => {
      if (controllerRef.current?.requiresDraftPersistence) { event.preventDefault(); event.returnValue = ""; }
    };
    window.addEventListener("pagehide", persist);
    window.addEventListener("online", retry);
    window.addEventListener("beforeunload", beforeUnload);
    return () => { window.removeEventListener("pagehide", persist); window.removeEventListener("online", retry); window.removeEventListener("beforeunload", beforeUnload); };
  }, []);

  function updateNote(title, body) {
    setNoteTitle(title);
    setMarkdown(body);
    controllerRef.current?.edit({ displayName: title.trim() || "Untitled", markdown: body });
  }

  function recoverDraft(record) {
    const confirmed = confirmedNoteRef.current;
    if (!confirmed) return;
    setNoteTitle(record.document.displayName);
    setMarkdown(record.document.markdown);
    setDrafts([]);
    beginController(confirmed.note, confirmed.document, loadEpochRef.current, record);
    void controllerRef.current.flush();
  }

  async function compareRemote() {
    const epoch = loadEpochRef.current;
    const id = controllerRef.current?.state.draft.noteId;
    if (!id) return;
    try {
      const remote = await api(`/api/library/${encodeURIComponent(id)}/note`);
      if (epoch === loadEpochRef.current) setComparison(remote);
    } catch (requestError) {
      if (epoch === loadEpochRef.current) setError(requestError.message);
    }
  }

  function resubmit() {
    if (!comparison) return;
    controllerRef.current.rebase(comparison.object.contentGeneration);
    setComparison(null);
    void controllerRef.current.flush();
  }

  function saveCopy() {
    const draft = controllerRef.current?.state.draft.document;
    if (!draft) return;
    navigate(`${base}/library/new?${new URLSearchParams({ draft: crypto.randomUUID().replaceAll("-", ""), ...(folderId ? { folder: folderId } : {}) })}`, {
      state: { noteDraft: { name: t("libraryObjectRoute.copyTitle", { title: draft.displayName }), markdown: draft.markdown } },
    });
  }

  async function deleteItem() {
    if (!item || deleting) return;
    setDeleting(true);
    try {
      await api(`/api/library/${item.id}`, { method: "DELETE" });
      backToLibrary();
    } catch (requestError) {
      setError(t("libraryObjectRoute.unableToMoveThisItemToTrashValue", { value1: requestError.message }));
    } finally {
      setDeleting(false);
    }
  }

  const isNote = item?.objectKind === "note";
  const isOffice = Boolean(officeFileType(item?.displayName));
  const isSpreadsheet = /\.xlsx$/i.test(item?.displayName || "");
  const isMarkdown = Boolean(item && !isNote && isMarkdownFile(item));
  const isCode = Boolean(item && !isNote && !isOffice && !isMarkdown && codePreviewCanRender(item.displayName, item.contentType));
  const displayedTitle = isNote ? noteTitle || t("libraryObjectRoute.untitled") : item?.displayName;

  return <><div className={`libraryPreviewMain${isOffice ? " libraryOfficePreview" : ""}${isSpreadsheet ? " librarySpreadsheetPreviewPage" : ""}${isCode ? " libraryCodePreviewPage" : ""}`}>
    <span className="srOnly" role="status" aria-live="polite">{saveStatus}</span>
    <header className="libraryPreviewHeader">
      {isNote ? <nav className="libraryNoteIdentity" aria-label={t("libraryObjectRoute.notePath")}>
        <button type="button" onClick={backToLibrary}>{t("agentRoute.private")}</button>
        <span aria-hidden="true">/</span>
        {renamingTitle
          ? <input
            autoFocus
            value={noteTitle}
            onChange={(event) => updateNote(event.target.value, markdown)}
            onBlur={() => setRenamingTitle(false)}
            onKeyDown={(event) => {
              if (event.key !== "Enter" && event.key !== "Escape") return;
              event.preventDefault();
              setRenamingTitle(false);
            }}
            aria-label={t("libraryObjectRoute.noteTitle")}
            placeholder={t("libraryObjectRoute.untitled")}
          />
          : <button className="libraryNoteName" type="button" onClick={() => setRenamingTitle(true)} aria-label={t("libraryObjectRoute.renameNote")}>{displayedTitle}</button>}
        <small><LockKeyhole aria-hidden="true" />{t("agentRoute.private")}</small>
        <small>{saveStatus}</small>
        <button type="button" disabled={loading || saveState?.status === "syncing" || saveState?.status === "conflict"} onClick={() => void controllerRef.current?.flush()}>{t("libraryObjectRoute.saveNow")}</button>
      </nav> : <nav aria-label={t("libraryObjectRoute.libraryPath")}><button type="button" onClick={backToLibrary}><ArrowLeft aria-hidden="true" />{t("libraryObjectRoute.library")}</button>{item && <><span>/</span><strong>{displayedTitle}</strong></>}</nav>}
      {item && !isNote && <div className="libraryPreviewActions"><a href={apiUrl(`/api/library/${item.id}/download`)} aria-label={t("workspaceContextPanel.download")}><Download aria-hidden="true" /></a><button type="button" disabled={deleting} onClick={() => void deleteItem()} aria-label={t("agentRoute.moveToTrash")}><Trash2 aria-hidden="true" /></button></div>}
    </header>
    <section className={`libraryPreviewBody${isNote ? " libraryNotePreview" : ""}${isMarkdown ? " isMarkdown" : ""}${isCode ? " isCode" : ""}`}>
      {loading ? <div className="libraryEmptyState" role="status" aria-live="polite"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryObjectRoute.openingFile")}</div> : <>{error && <div className="errorBanner libraryError" role="alert">{error}<button type="button" onClick={() => { if (controllerRef.current) void controllerRef.current.flush(); else void load(); }}>{t("libraryObjectRoute.retry")}</button></div>}{isNote ? <div className="libraryNoteEditor">
        {drafts.length > 0 && <div className="errorBanner" role="status"><span>{t("libraryObjectRoute.draftsAvailable")}</span>{drafts.map(record => <button type="button" key={record.key} onClick={() => recoverDraft(record)}>{t("libraryObjectRoute.recoverDraft", { title: record.document.displayName, time: new Date(record.updatedAt).toLocaleString() })}</button>)}<button type="button" onClick={() => setDrafts([])}>{t("libraryObjectRoute.useServerVersion")}</button></div>}
        {saveState?.status === "conflict" && <div className="errorBanner" role="alert"><span>{t("libraryObjectRoute.conflictHelp")}</span><button type="button" onClick={() => void compareRemote()}>{t("libraryObjectRoute.compare")}</button><button type="button" onClick={saveCopy}>{t("libraryObjectRoute.saveCopy")}</button>{comparison && <button type="button" onClick={resubmit}>{t("libraryObjectRoute.resubmit")}</button>}</div>}
        {comparison && <section aria-label={t("libraryObjectRoute.compare")}><label>{t("libraryObjectRoute.localVersion")}<strong>{noteTitle}</strong><textarea readOnly value={markdown} /></label><label>{t("libraryObjectRoute.serverVersion")}<strong>{comparison.object.displayName}</strong><textarea readOnly value={comparison.markdown} /></label></section>}
        <textarea value={markdown} onChange={(event) => updateNote(noteTitle, event.target.value)} onKeyDown={(event) => { if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") { event.preventDefault(); void controllerRef.current?.flush(); } }} aria-label={t("libraryObjectRoute.noteContent")} placeholder={t("libraryObjectRoute.startWritingOrTypeForCommands")} /></div> : error ? null : isImage(item) ? <img className="libraryPreviewImage" src={apiUrl(`/api/library/${item.id}/preview`)} alt={item.displayName} /> : isSpreadsheet ? <SpreadsheetPreview className="libraryPreviewFrame" src={apiUrl(`/api/spreadsheet-preview/userLibraryObject/${item.id}`)} printSrc={officePreviewUrl("userLibraryObject", item.id)} title={item.displayName} /> : canEmbed(item) ? <DocumentPreview className={isMarkdown ? "libraryMarkdownPreview" : isCode ? "libraryCodePreview" : "libraryPreviewFrame"} src={officeFileType(item.displayName) ? officePreviewUrl("userLibraryObject", item.id) : apiUrl(`/api/library/${item.id}/preview`)} title={item.displayName} contentType={item.contentType} /> : <div className="libraryPreviewUnsupported"><FileText aria-hidden="true" /><strong>{item.displayName}</strong><span>{t("libraryObjectRoute.onlinePreviewIsUnavailableForThisFileType")}</span><a href={apiUrl(`/api/library/${item.id}/download`)}><Download aria-hidden="true" />{t("libraryObjectRoute.downloadFile")}</a></div>}</>}
    </section>
  </div><ConfirmDialog open={blocker.state === "blocked"} title={t("libraryObjectRoute.leaveTitle")} message={t("libraryObjectRoute.leaveHelp")} confirmLabel={t("libraryObjectRoute.leaveWithoutDraft")} cancelLabel={t("libraryObjectRoute.stay")} onCancel={() => blocker.reset()} onConfirm={() => blocker.proceed()} /></>;
}

export default function LibraryPreviewPage() {
  useTranslation();
  return <ShellPage><LibraryPreviewPageContent /></ShellPage>;
}
