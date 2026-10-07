
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { useLocation, useNavigate, useParams, useRouteLoaderData, useSearchParams } from "react-router";
import { ArrowLeft, Download, FileText, LoaderCircle, LockKeyhole, Trash2 } from "lucide-react";
import { apiJson as api, apiUrl, jsonOptions } from "../api";
import { ShellPage } from "../shell/ShellPage";
import { DocumentPreview } from "../components/DocumentPreview";
import { officeFileType } from "../chat/officeFormats.mjs";
import { officePreviewUrl } from "../chat/attachments.mjs";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import SpreadsheetPreview from "../components/SpreadsheetPreview";

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

function splitNote(markdown, fallbackTitle) {
  const heading = markdown.match(/^#\s+([^\n]+)\n?/);
  if (!heading) return { title: fallbackTitle === "Untitled" ? "" : fallbackTitle, body: markdown };
  return { title: heading[1].trim(), body: markdown.slice(heading[0].length).replace(/^\n/, "") };
}

function composeNote(title, body) {
  const cleanTitle = title.trim();
  return cleanTitle ? `# ${cleanTitle}${body ? `\n\n${body}` : ""}` : body;
}

function LibraryPreviewPageContent() {
  const { t } = useTranslation();
  const params = useParams();
  const location = useLocation();
  const navigate = useNavigate();
  const { workspace } = useRouteLoaderData("workspace");
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
  const [saveStatus, setSaveStatus] = useState("");
  const noteRef = useRef({ displayName: "Untitled", markdown: "" });
  const itemIdRef = useRef("");
  const saveTimerRef = useRef(null);
  const saveQueueRef = useRef(Promise.resolve());

  function backToLibrary() {
    navigate(folderId ? `${base}/library?folder=${encodeURIComponent(folderId)}` : `${base}/library`);
  }

  async function load() {
    setLoading(true);
    setError("");
    setSaveStatus("");
    if (libraryObjectId === "new") {
      const draft = location.state?.noteDraft || { name: "Untitled", markdown: "" };
      const note = splitNote(draft.markdown || "", draft.name || "Untitled");
      noteRef.current = { displayName: note.title || "Untitled", markdown: draft.markdown || "" };
      itemIdRef.current = "";
      setItem({ id: "", objectKind: "note", contentType: "text/markdown", displayName: noteRef.current.displayName, status: "draft" });
      setNoteTitle(note.title);
      setMarkdown(note.body);
      setRenamingTitle(true);
      setLoading(false);
      return;
    }
    try {
      const result = await api(`/api/library/${libraryObjectId}`);
      if (result.object.objectKind === "folder") {
        navigate(`${base}/library?folder=${encodeURIComponent(result.object.id)}`, { replace: true });
        return;
      }
      itemIdRef.current = result.object.id;
      setItem(result.object);
      if (result.object.objectKind === "note") {
        const note = await api(`/api/library/${libraryObjectId}/note`);
        const fields = splitNote(note.markdown, result.object.displayName);
        noteRef.current = { displayName: result.object.displayName, markdown: note.markdown };
        setNoteTitle(fields.title);
        setMarkdown(fields.body);
        setRenamingTitle(false);
      }
    } catch (requestError) {
      setError(t("libraryObjectRoute.unableToOpenFileValue", { value1: requestError.message }));
    } finally {
      setLoading(false);
    }
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: Object identity is the load trigger; the render-local command must not retrigger itself.
  useEffect(() => {
    load();
  }, [libraryObjectId]);

  useEffect(() => () => window.clearTimeout(saveTimerRef.current), []);

  function saveNote() {
    if (!item) return Promise.resolve();
    const snapshot = { ...noteRef.current };
    if (!itemIdRef.current && !snapshot.markdown.trim()) return Promise.resolve();
    const operation = saveQueueRef.current.then(async () => {
      setError("");
      setSaveStatus(t("libraryObjectRoute.saving"));
      try {
        const result = itemIdRef.current
          ? await api(`/api/library/${itemIdRef.current}/note`, {
            method: "PUT",
            body: JSON.stringify(snapshot),
            headers: { "Content-Type": "application/json" },
          })
          : await api("/api/library/notes", jsonOptions("POST", {
            displayName: snapshot.displayName,
            markdown: snapshot.markdown,
            parentFolderId: folderId,
          }));
        const created = !itemIdRef.current;
        itemIdRef.current = result.object.id;
        setItem(result.object);
        if (created) {
          navigate(`${base}/library/${encodeURIComponent(result.object.id)}${folderId ? `?folder=${encodeURIComponent(folderId)}` : ""}`, { replace: true, state: null });
        }
        setSaveStatus(t("libraryObjectRoute.saved"));
      } catch (requestError) {
        setError(t("libraryObjectRoute.unableToSaveValue", { value1: requestError.message }));
        setSaveStatus(t("libraryObjectRoute.unableToSave"));
        throw requestError;
      }
    });
    saveQueueRef.current = operation.catch(() => undefined);
    return operation;
  }

  function scheduleSave() {
    window.clearTimeout(saveTimerRef.current);
    saveTimerRef.current = window.setTimeout(() => { void saveNote().catch(() => {}); }, 600);
  }

  function updateNote(title, body) {
    setNoteTitle(title);
    setMarkdown(body);
    setSaveStatus(t("libraryObjectRoute.youHaveUnsavedChanges"));
    noteRef.current = { displayName: title.trim() || "Untitled", markdown: composeNote(title, body) };
    scheduleSave();
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

  return <div className={`libraryPreviewMain${isOffice ? " libraryOfficePreview" : ""}${isSpreadsheet ? " librarySpreadsheetPreviewPage" : ""}${isCode ? " libraryCodePreviewPage" : ""}`}>
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
      </nav> : <nav aria-label={t("libraryObjectRoute.libraryPath")}><button type="button" onClick={backToLibrary}><ArrowLeft aria-hidden="true" />{t("libraryObjectRoute.library")}</button>{item && <><span>/</span><strong>{displayedTitle}</strong></>}</nav>}
      {item && !isNote && <div className="libraryPreviewActions"><a href={apiUrl(`/api/library/${item.id}/download`)} aria-label={t("workspaceContextPanel.download")}><Download aria-hidden="true" /></a><button type="button" disabled={deleting} onClick={() => void deleteItem()} aria-label={t("agentRoute.moveToTrash")}><Trash2 aria-hidden="true" /></button></div>}
    </header>
    <section className={`libraryPreviewBody${isNote ? " libraryNotePreview" : ""}${isMarkdown ? " isMarkdown" : ""}${isCode ? " isCode" : ""}`}>
      {loading ? <div className="libraryEmptyState" role="status" aria-live="polite"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryObjectRoute.openingFile")}</div> : <>{error && <div className="errorBanner libraryError" role="alert">{error}<button type="button" onClick={load}>{t("libraryObjectRoute.retry")}</button></div>}{isNote ? <div className="libraryNoteEditor"><textarea value={markdown} onChange={(event) => updateNote(noteTitle, event.target.value)} aria-label={t("libraryObjectRoute.noteContent")} placeholder={t("libraryObjectRoute.startWritingOrTypeForCommands")} /></div> : error ? null : isImage(item) ? <img className="libraryPreviewImage" src={apiUrl(`/api/library/${item.id}/preview`)} alt={item.displayName} /> : isSpreadsheet ? <SpreadsheetPreview className="libraryPreviewFrame" src={apiUrl(`/api/spreadsheet-preview/userLibraryObject/${item.id}`)} printSrc={officePreviewUrl("userLibraryObject", item.id)} title={item.displayName} /> : canEmbed(item) ? <DocumentPreview className={isMarkdown ? "libraryMarkdownPreview" : isCode ? "libraryCodePreview" : "libraryPreviewFrame"} src={officeFileType(item.displayName) ? officePreviewUrl("userLibraryObject", item.id) : apiUrl(`/api/library/${item.id}/preview`)} title={item.displayName} contentType={item.contentType} /> : <div className="libraryPreviewUnsupported"><FileText aria-hidden="true" /><strong>{item.displayName}</strong><span>{t("libraryObjectRoute.onlinePreviewIsUnavailableForThisFileType")}</span><a href={apiUrl(`/api/library/${item.id}/download`)}><Download aria-hidden="true" />{t("libraryObjectRoute.downloadFile")}</a></div>}</>}
    </section>
  </div>;
}

export default function LibraryPreviewPage() {
  useTranslation();
  return <ShellPage><LibraryPreviewPageContent /></ShellPage>;
}
