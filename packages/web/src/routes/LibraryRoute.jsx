import { i18n } from "../i18n";
import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useRouteLoaderData, useSearchParams } from "react-router";
import {
  Bot, BookOpen, ChevronDown, ChevronRight, Download, FileText, Folder, FolderInput,
  Image as ImageIcon, Layers, LoaderCircle, MessageSquarePlus, Minus,
  Search, Trash2, Upload, X,
} from "lucide-react";
import { apiJson as api, apiUrl, jsonOptions } from "../api";
import { OperationClient } from "../chat/operationReceipts";
import { MarkdownContent } from "../chat/MarkdownContent";
import { useModalDialog } from "../components/useModalDialog";
import { AgentMark } from "../shell/AgentMark";
import { ShellPage } from "../shell/ShellPage";
import { MAX_UPLOAD_BATCH_FILES } from "../upload";

function isImage(item) {
  return item.objectKind === "image" || item.contentType.startsWith("image/");
}

function isFolder(item) {
  return item.objectKind === "folder";
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatDate(value) {
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "—";
  return new Intl.DateTimeFormat(i18n.resolvedLanguage, { month: "short", day: "numeric" }).format(date);
}

function statusLabel(item) {
  if (item.status === "ready") return "";
  if (item.status === "processing") return t("libraryRoute.processing");
  if (item.status === "failed") return t("libraryRoute.processingFailed");
  throw new Error(`unsupported library status: ${item.status}`);
}

const LIBRARY_VIEWS = new Set(["materials", "knowledge", "agents", "skills"]);
const LIBRARY_TABS = () => ([
  ["materials", t("libraryRoute.materials"), Folder],
  ["knowledge", t("libraryRoute.knowledgeBases"), BookOpen],
  ["agents", t("agentRunRow.agent"), Bot],
  ["skills", "Skills", Layers],
]);

function SkillMarkdown({ content }) {
  useTranslation();
  const source = content.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "");
  return <MarkdownContent text={source} />;
}

function LibraryPageContent() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { workspace, agents } = useRouteLoaderData("workspace");
  const { user } = useRouteLoaderData("authenticated");
  const operations = useMemo(() => new OperationClient({ userId: user.id, workspaceId: workspace.id, command: "createSession" }), [user.id, workspace.id]);
  const operationScopeRef = useRef({ operations, active: true });
  if (operationScopeRef.current.operations !== operations) operationScopeRef.current = { operations, active: true };
  useEffect(() => {
    const scope = operationScopeRef.current;
    if (scope.operations !== operations) return undefined;
    scope.active = true;
    return () => { scope.active = false; };
  }, [operations]);
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const [searchParams] = useSearchParams();
  const fileInputRef = useRef(null);
  const libraryView = searchParams.get("view") || "materials";
  const folderId = searchParams.get("folder") || "";
  const [objects, setObjects] = useState([]);
  const [skills, setSkills] = useState(null);
  const [selectedSkillId, setSelectedSkillId] = useState("");
  const [skillDetail, setSkillDetail] = useState(null);
  const [skillDetailLoading, setSkillDetailLoading] = useState(false);
  const [skillDetailError, setSkillDetailError] = useState("");
  const [catalogError, setCatalogError] = useState("");
  const [folderPath, setFolderPath] = useState([]);
  const [filter, setFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [newMenuOpen, setNewMenuOpen] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [uploadQueue, setUploadQueue] = useState([]);
  const [uploadDialogError, setUploadDialogError] = useState("");
  const [uploadDragActive, setUploadDragActive] = useState(false);
  const [selectedIds, setSelectedIds] = useState([]);
  const [dialog, setDialog] = useState(() => searchParams.get("upload") === "1" ? "upload" : "");
  const [dialogTitle, setDialogTitle] = useState("");
  const [moveFolderId, setMoveFolderId] = useState("");
  const [moveObjects, setMoveObjects] = useState([]);
  const [moveLoading, setMoveLoading] = useState(false);
  const [creatingMoveFolder, setCreatingMoveFolder] = useState(false);
  const [moveFolderName, setMoveFolderName] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState("");
  const dialogRef = useModalDialog({ open: Boolean(dialog), busy: working || uploading, onClose: closeDialog });

  if (!LIBRARY_VIEWS.has(libraryView)) throw new Error(`unsupported library view: ${libraryView}`);

  const selectedItems = objects.filter((item) => selectedIds.includes(item.id));

  function libraryUrl(nextFolderId = "") {
    return nextFolderId ? `${base}/library?folder=${encodeURIComponent(nextFolderId)}` : `${base}/library`;
  }

  async function loadFolderPath(currentFolderId) {
    if (!currentFolderId) return [];
    const path = [];
    let nextId = currentFolderId;
    while (nextId) {
      const result = await api(`/api/library/${nextId}`);
      path.unshift(result.object);
      nextId = result.object.parentFolderId || "";
    }
    return path;
  }

  async function loadLibrary() {
    setLoading(true);
    setError("");
    try {
      const suffix = folderId ? `?parentFolderId=${encodeURIComponent(folderId)}` : "";
      const [library, path] = await Promise.all([
        api(`/api/library${suffix}`), loadFolderPath(folderId),
      ]);
      setObjects(library.objects);
      setFolderPath(path);
      setSelectedIds([]);
    } catch (requestError) {
      if (requestError.status === 404 && folderId) {
        navigate(`${base}/library`, { replace: true });
        return;
      }
      setError(t("libraryRoute.unableToLoadLibraryValue", { value1: requestError.message }));
    } finally {
      setLoading(false);
    }
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: The primitive folder route is the load trigger; the render-local command must not retrigger itself.
  useEffect(() => {
    loadLibrary();
  }, [folderId]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Changing catalogs intentionally clears view-local search and selection state.
  useEffect(() => {
    setQuery("");
    setSelectedIds([]);
    setCatalogError("");
    setSelectedSkillId("");
    setSkillDetail(null);
    setSkillDetailError("");
  }, [libraryView]);

  useEffect(() => {
    if (libraryView !== "skills") return undefined;
    let active = true;
    setSkills(null);
    api(`/api/workspaces/${workspace.id}/skills`)
      .then((result) => {
        if (active) setSkills(result.skills);
      })
      .catch((requestError) => {
        if (active) setCatalogError(t("libraryRoute.unableToLoadSkillsValue", { value1: requestError.message }));
      });
    return () => { active = false; };
  }, [libraryView, workspace.id, t]);

  useEffect(() => {
    if (!selectedSkillId) return undefined;
    const closeOnEscape = (event) => {
      if (event.key === "Escape") setSelectedSkillId("");
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [selectedSkillId]);

  async function openSkill(skillId) {
    setSelectedSkillId(skillId);
    setSkillDetail(null);
    setSkillDetailLoading(true);
    setSkillDetailError("");
    try {
      const result = await api(`/api/workspaces/${workspace.id}/skills/${encodeURIComponent(skillId)}`);
      setSkillDetail(result);
    } catch (requestError) {
      setSkillDetailError(t("libraryRoute.unableToOpenSkillValue", { value1: requestError.message }));
    } finally {
      setSkillDetailLoading(false);
    }
  }

  function uploadFileKey(file) {
    return [file.name, file.size, file.lastModified, file.type].join("\u0000");
  }

  function addUploadFiles(incomingFiles) {
    const files = Array.from(incomingFiles || []);
    if (!files.length || uploading) return;
    const existingKeys = new Set(uploadQueue.map(uploadFileKey));
    const nextKeys = new Set();
    const duplicate = files.find((file) => {
      const key = uploadFileKey(file);
      if (existingKeys.has(key) || nextKeys.has(key)) return true;
      nextKeys.add(key);
      return false;
    });
    if (duplicate) {
      setUploadDialogError(t("libraryRoute.valueIsAlreadyInTheUploadQueue", { value1: duplicate.name }));
      return;
    }
    if (uploadQueue.length + files.length > MAX_UPLOAD_BATCH_FILES) {
      setUploadDialogError(t("libraryRoute.uploadUpToValueFilesAtATime", { value1: MAX_UPLOAD_BATCH_FILES }));
      return;
    }
    setUploadDialogError("");
    setUploadQueue((current) => [...current, ...files]);
  }

  function selectUploadFile(event) {
    addUploadFiles(event.target.files);
    event.target.value = "";
  }

  function openUploadDialog() {
    setNewMenuOpen(false);
    setUploadQueue([]);
    setUploadDialogError("");
    setUploadDragActive(false);
    setDialog("upload");
  }

  function closeUploadDialog() {
    if (uploading) return;
    setUploadQueue([]);
    setUploadDialogError("");
    setUploadDragActive(false);
    setDialog("");
  }

  function closeDialog() {
    if (dialog === "upload") closeUploadDialog();
    else if (!working) setDialog("");
  }

  function removeUploadFile(file) {
    if (uploading) return;
    const targetKey = uploadFileKey(file);
    setUploadQueue((current) => current.filter((item) => uploadFileKey(item) !== targetKey));
    setUploadDialogError("");
  }

  async function uploadQueuedFiles() {
    const files = [...uploadQueue];
    if (!files.length || uploading) return;
    setUploading(true);
    setUploadDialogError("");
    try {
      const body = new FormData();
      files.forEach((file) => body.append("files", file));
      if (folderId) body.append("parentFolderId", folderId);
      const result = await api("/api/library", { method: "POST", body });
      if (
        !Array.isArray(result.objects)
        || result.objects.length !== files.length
        || result.objects.some((item, index) => !item?.id || item.displayName !== files[index].name)
        || new Set(result.objects.map((item) => item.id)).size !== files.length
      ) {
        throw new Error("library_upload_response_invalid");
      }
      setUploadQueue([]);
      setDialog("");
      await loadLibrary();
    } catch (requestError) {
      setUploadDialogError(t("libraryRoute.uploadFailedValue", { value1: requestError.message }));
    } finally {
      setUploading(false);
    }
  }

  async function createFolder() {
    if (!dialogTitle.trim() || working) return;
    setWorking(true);
    try {
      await api("/api/library/folders", jsonOptions("POST", { displayName: dialogTitle, parentFolderId: folderId || null }));
      setDialog("");
      await loadLibrary();
    } catch (requestError) {
      setError(t("libraryRoute.unableToCreateFolderValue", { value1: requestError.message }));
    } finally {
      setWorking(false);
    }
  }

  function createNote() {
    setNewMenuOpen(false);
    navigate(`${base}/library/new${folderId ? `?folder=${encodeURIComponent(folderId)}` : ""}`, {
      state: { noteDraft: { name: "Untitled", markdown: "" } },
    });
  }

  async function loadMoveDirectory(nextFolderId = "") {
    setMoveLoading(true);
    try {
      const suffix = nextFolderId ? `?parentFolderId=${encodeURIComponent(nextFolderId)}` : "";
      const result = await api(`/api/library${suffix}`);
      setMoveFolderId(nextFolderId);
      setMoveObjects(result.objects);
    } catch (requestError) {
      setError(t("libraryRoute.unableToLoadDestinationFolderValue", { value1: requestError.message }));
    } finally {
      setMoveLoading(false);
    }
  }

  async function openMoveDialog() {
    if (!selectedItems.length || working) return;
    setError("");
    setCreatingMoveFolder(false);
    setMoveFolderName("");
    setDialog("move");
    await loadMoveDirectory("");
  }

  async function moveSelected() {
    if (!selectedItems.length || working) return;
    setWorking(true);
    try {
      await Promise.all(selectedItems.map((item) => api(`/api/library/${item.id}`, jsonOptions("PATCH", { parentFolderId: moveFolderId || null }))));
      setDialog("");
      await loadLibrary();
    } catch (requestError) {
      setError(t("libraryRoute.unableToMoveValue", { value1: requestError.message }));
    } finally {
      setWorking(false);
    }
  }

  async function createMoveFolder() {
    if (!moveFolderName.trim() || working) return;
    setWorking(true);
    try {
      await api("/api/library/folders", jsonOptions("POST", { displayName: moveFolderName, parentFolderId: moveFolderId || null }));
      setMoveFolderName("");
      setCreatingMoveFolder(false);
      await loadMoveDirectory(moveFolderId);
    } catch (requestError) {
      setError(t("libraryRoute.unableToCreateFolderValue", { value1: requestError.message }));
    } finally {
      setWorking(false);
    }
  }

  async function deleteSelected() {
    if (!selectedItems.length || working) return;
    setWorking(true);
    try {
      const results = await Promise.allSettled(selectedItems.map((item) => api(`/api/library/${item.id}`, { method: "DELETE" })));
      const failures = results.filter((result) => result.status === "rejected");
      await loadLibrary();
      if (failures.length) {
        const nonEmpty = failures.some((result) => result.reason?.message === "library_folder_not_empty");
        setError(nonEmpty
          ? t("libraryRoute.valueItemsMovedToTrashValueNonEmptyFolders", { value1: results.length - failures.length, value2: failures.length })
          : t("libraryRoute.valueItemsMovedToTrashValueFailedPleaseTry", { value1: results.length - failures.length, value2: failures.length }));
      }
    } finally {
      setWorking(false);
    }
  }

  async function createChat(agentId) {
    setDialog("");
    if (!selectedItems.length || selectedItems.some(isFolder) || !workspace || working) return;
    setWorking(true);
    const scope = operationScopeRef.current;
    const isCurrent = () => scope.active && operationScopeRef.current === scope;
    try {
      const accepted = await operations.submit(`/api/workspaces/${workspace.id}/sessions`, { agentId }, [], selectedItems.map((item) => item.id).sort());
      await finishCreatedChat(accepted, isCurrent);
    } catch (requestError) {
      if (!isCurrent()) return;
      setError(requestError?.message === "operation_pending_input_changed" ? t("operations.changedInput") : t("libraryRoute.unableToStartChatValue", { value1: requestError.message }));
    } finally {
      if (isCurrent()) setWorking(false);
    }
  }

  async function finishCreatedChat(accepted, isCurrent, linkAssets = true) {
    if (!isCurrent()) return;
    const pending = operations.pending();
    if (pending?.operationId !== accepted.operationId) throw new Error("operation_identity_changed");
    const { session } = await api(`/api/sessions/${encodeURIComponent(accepted.sessionId)}`);
    if (!isCurrent()) return;
    if (session?.id !== accepted.sessionId || typeof session.agentId !== "string") throw new Error("session_response_invalid");
    if (linkAssets) await Promise.all(pending.assetIds.map((assetId) => api(`/api/sessions/${accepted.sessionId}/assets`, jsonOptions("POST", {
      assetKind: "userLibraryObject", assetId,
    }))));
    if (!isCurrent()) return;
    operations.complete(accepted.operationId);
    navigate(`${base}/agents/${encodeURIComponent(session.agentId)}?sessionId=${encodeURIComponent(session.id)}`);
  }

  async function openPendingChat(linkAssets = true) {
    const scope = operationScopeRef.current;
    const isCurrent = () => scope.active && operationScopeRef.current === scope;
    setWorking(true);
    setError("");
    try {
      const accepted = await operations.recover();
      if (accepted) await finishCreatedChat(accepted, isCurrent, linkAssets);
    } catch { if (isCurrent()) setError(t("operations.acceptedReadFailed")); }
    finally { if (isCurrent()) setWorking(false); }
  }

  function startChat() {
    if (agents.length === 1) void createChat(agents[0].id);
    else if (agents.length > 1) setDialog("agent");
    else navigate(`${base}/app`);
  }

  const visibleObjects = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    return objects.filter((item) => {
      if (filter === "images" && !isImage(item)) return false;
      if (filter === "files" && (isImage(item) || isFolder(item))) return false;
      return !normalizedQuery || item.displayName.toLocaleLowerCase().includes(normalizedQuery);
    });
  }, [filter, objects, query]);
  const normalizedCatalogQuery = query.trim().toLocaleLowerCase();
  const visibleAgents = agents.filter((agent) => !normalizedCatalogQuery || `${agent.name} ${agent.description}`.toLocaleLowerCase().includes(normalizedCatalogQuery));
  const visibleSkills = (skills || []).filter((skill) => !normalizedCatalogQuery || `${skill.name} ${skill.description}`.toLocaleLowerCase().includes(normalizedCatalogQuery));
  const visibleObjectIds = visibleObjects.map((item) => item.id);
  const allVisibleSelected = visibleObjectIds.length > 0 && visibleObjectIds.every((itemId) => selectedIds.includes(itemId));
  const selectionContainsFolder = selectedItems.some(isFolder);

  function toggleSelection(itemId) {
    setSelectedIds((current) => current.includes(itemId) ? current.filter((currentId) => currentId !== itemId) : [...current, itemId]);
  }

  function toggleAllVisible() {
    setSelectedIds((current) => allVisibleSelected
      ? current.filter((itemId) => !visibleObjectIds.includes(itemId))
      : [...new Set([...current, ...visibleObjectIds])]);
  }

  function downloadSelected() {
    selectedItems.forEach((item) => {
      const link = document.createElement("a");
      link.href = apiUrl(`/api/library/${item.id}/download`);
      link.download = item.displayName;
      document.body.append(link);
      link.click();
      link.remove();
    });
  }

  return (
    <ShellPage>
      <div className={`libraryWorkspace ${selectedSkillId ? "hasSkillPeek" : ""}`}>
        <section className="libraryMain shEmbeddedLibrary" aria-labelledby="library-title">
        <header className="libraryHeader">
          <h1 id="library-title">{t("workspaceContextPanel.library")}</h1>
          {libraryView === "materials" ? <div className="libraryHeaderActions">
            <div className="libraryNewMenu">
              <button className="libraryNewButton" type="button" aria-expanded={newMenuOpen} aria-haspopup="menu" onClick={() => setNewMenuOpen((open) => !open)}>{t("libraryRoute.new")}<ChevronDown aria-hidden="true" /></button>
              {newMenuOpen && <div className="libraryNewMenuPanel" role="menu">
                <button type="button" role="menuitem" disabled={uploading} onClick={openUploadDialog}><Upload aria-hidden="true" />{t("libraryRoute.uploadFiles")}</button>
                <button type="button" role="menuitem" onClick={() => { setNewMenuOpen(false); setDialogTitle(""); setDialog("folder"); }}><Folder aria-hidden="true" />{t("libraryRoute.folder")}</button>
                <button type="button" role="menuitem" onClick={createNote}><FileText aria-hidden="true" />{t("libraryRoute.note")}</button>
              </div>}
            </div>
          </div> : null}
        </header>

        <div className="libraryContent">
          <nav className="libraryCatalogTabs" role="tablist" aria-label={t("libraryRoute.libraryType")}>
            {LIBRARY_TABS().map(([value, label, Icon]) => <Link
              className={libraryView === value ? "isActive" : ""}
              role="tab"
              aria-selected={libraryView === value}
              to={value === "materials" ? `${base}/library` : `${base}/library?view=${value}`}
              key={value}
            ><Icon aria-hidden="true" />{label}</Link>)}
          </nav>

          <div className="libraryCatalogTools">
            {libraryView === "materials" && selectedItems.length ? <div className="librarySelectionBar" aria-label={t("libraryRoute.selectedFileActions")}>
              <span>{t("library.selected", { count: selectedItems.length })}</span>
              <button className="primary" type="button" disabled={selectionContainsFolder || working} onClick={startChat}><MessageSquarePlus aria-hidden="true" />{t("libraryRoute.startChat")}</button>
              <button type="button" disabled={selectionContainsFolder || working} onClick={downloadSelected}><Download aria-hidden="true" />{t("workspaceContextPanel.download")}</button>
              <button type="button" disabled={working} onClick={openMoveDialog}><FolderInput aria-hidden="true" />{t("libraryRoute.move")}</button>
              <button className="danger" type="button" disabled={working} onClick={() => void deleteSelected()}><Trash2 aria-hidden="true" />{t("agentRoute.moveToTrash")}</button>
            </div> : libraryView === "materials" ? <div className="libraryFilters" role="tablist" aria-label={t("libraryRoute.materialType")}>
              {[["all", t("libraryRoute.all")], ["images", t("attachmentCard.image")], ["files", t("libraryRoute.files")]].map(([value, label]) => <button className={filter === value ? "active" : ""} type="button" key={value} role="tab" aria-selected={filter === value} onClick={() => setFilter(value)}>{label}</button>)}
            </div> : <span className="libraryCatalogAll">{t("libraryRoute.all")}</span>}
            <label className="librarySearch"><Search aria-hidden="true" /><span className="srOnly">{t("libraryRoute.searchThisLibrary")}</span><input aria-label={t("libraryRoute.searchThisLibrary")} value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t("libraryRoute.search")} /></label>
          </div>

          {libraryView === "materials" ? <>
            {folderPath.length > 0 && <nav className="libraryBreadcrumb" aria-label={t("libraryRoute.currentFolder")}><button type="button" onClick={() => navigate(`${base}/library`)}>{t("libraryObjectRoute.library")}</button>{folderPath.map((folder) => <span key={folder.id}><ChevronRight aria-hidden="true" /><button type="button" onClick={() => navigate(libraryUrl(folder.id))}>{folder.displayName}</button></span>)}</nav>}
            {error && <div className="errorBanner libraryError" role="alert">{error}<button type="button" disabled={working} onClick={() => operations.pending() ? void openPendingChat() : void loadLibrary()}>{t("libraryObjectRoute.retry")}</button></div>}
            <div className={`libraryList ${selectedItems.length ? "hasSelection" : ""}`} role="table" aria-label={t("libraryRoute.personalLibraryFiles")}>
              <div className="libraryListHeader" role="row">
                <button className="librarySelect librarySelectAll" type="button" role="checkbox" aria-checked={allVisibleSelected} aria-label={allVisibleSelected ? t("libraryRoute.deselectAll") : t("libraryRoute.selectAllListedItems")} onClick={toggleAllVisible}>{allVisibleSelected ? <Minus aria-hidden="true" /> : null}</button>
                <span role="columnheader">{t("libraryRoute.name")}</span><span role="columnheader">{t("libraryRoute.modified")}</span><span role="columnheader">{t("libraryRoute.size")}</span>
              </div>
              {loading ? <div className="libraryEmptyState" role="status" aria-live="polite"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryRoute.loadingLibrary")}</div> : visibleObjects.length === 0 ? <div className="libraryEmptyState">{objects.length === 0 ? t("libraryRoute.noMaterialsYetConversationAttachmentsWillBeSavedHere") : t("libraryRoute.noMaterialsMatchYourFilters")}</div> : visibleObjects.map((item) => {
                const ItemIcon = isFolder(item) ? Folder : isImage(item) ? ImageIcon : FileText;
                const itemStatus = statusLabel(item);
                return <div className={`libraryRow ${selectedIds.includes(item.id) ? "selected" : ""}`} role="row" key={item.id}>
                  <label className="librarySelect"><input type="checkbox" checked={selectedIds.includes(item.id)} onChange={() => toggleSelection(item.id)} /><span className="srOnly">{t("libraryRoute.select")}{" "}{item.displayName}</span></label>
                  <button className="libraryName" type="button" role="cell" onClick={() => navigate(isFolder(item) ? libraryUrl(item.id) : `${base}/library/${item.id}?folder=${encodeURIComponent(folderId)}`)}>
                    {isImage(item) && item.status === "ready" ? <img className="libraryThumbnail" src={apiUrl(`/api/library/${item.id}/preview`)} alt="" loading="lazy" /> : <ItemIcon aria-hidden="true" />}<span>{item.displayName}</span>{itemStatus && <small>{itemStatus}</small>}
                  </button>
                  <span role="cell">{formatDate(item.updatedAt)}</span><span role="cell">{isFolder(item) ? "—" : formatBytes(item.sizeBytes)}</span>
                </div>;
              })}
            </div>
          </> : libraryView === "knowledge" ? <div className="libraryCatalogEmpty"><BookOpen aria-hidden="true" /><strong>{t("libraryRoute.noKnowledgeBasesYet")}</strong><span>{t("libraryRoute.connectedSourcesWillAppearHere")}</span></div>
            : libraryView === "agents" ? <div className="libraryAgentStrip" aria-label={t("agentRunRow.agent")}>
              {visibleAgents.map((agent) => <Link className="libraryAgentCard" to={`${base}/agents/${encodeURIComponent(agent.id)}`} key={agent.id}><AgentMark agent={agent} /><strong>{agent.name}</strong><small>{agent.description || t("libraryRoute.privateAgents")}</small></Link>)}
              <Link className="libraryAgentCard isAdd" to={`${base}/settings/agents?new=1`}><span aria-hidden="true">＋</span><strong>{t("libraryRoute.newAgent")}</strong></Link>
              {!visibleAgents.length && normalizedCatalogQuery ? <p className="libraryCatalogNoMatch">{t("libraryRoute.noMatchingAgents")}</p> : null}
            </div>
              : <>
                  {catalogError ? <div className="errorBanner libraryError" role="alert">{catalogError}</div> : null}
                  {skills === null && !catalogError ? <div className="libraryCatalogEmpty" role="status" aria-live="polite"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryRoute.loadingSkills")}</div> : null}
                  {skills?.length === 0 && !catalogError ? <div className="libraryCatalogEmpty"><Layers aria-hidden="true" /><strong>{t("libraryRoute.noSkillsAvailableInThisWorkspace")}</strong><span>{t("libraryRoute.enableAPluginContainingSkillsToSeeThemHere")}</span></div> : null}
                  {skills?.length ? <div className="librarySkillTable" role="table" aria-label="Skills">
                    <div className="librarySkillRow isHead" role="row"><span>{t("libraryRoute.name")}</span><span>{t("libraryRoute.description")}</span><span>{t("libraryRoute.source")}</span><span>{t("libraryRoute.invocation")}</span></div>
                    {visibleSkills.map((skill) => <button className={`librarySkillRow ${selectedSkillId === skill.skillId ? "isSelected" : ""}`} type="button" role="row" aria-label={t("attachmentCard.previewValue", { value1: skill.name })} onClick={() => void openSkill(skill.skillId)} key={skill.skillId}><span role="cell"><Layers aria-hidden="true" /><strong>{skill.name}</strong></span><span role="cell">{skill.description}</span><span role="cell">{skill.skillId.startsWith("system:") ? t("libraryRoute.system") : t("libraryRoute.plugins")}</span><span role="cell">{skill.allowImplicitInvocation ? t("appRoute.auto") : t("libraryRoute.explicit")}</span></button>)}
                    {!visibleSkills.length && normalizedCatalogQuery ? <p className="libraryCatalogNoMatch">{t("libraryRoute.noMatchingSkills")}</p> : null}
                  </div> : null}
                </>}
        </div>
        </section>

        <aside className={`librarySkillPeek ${selectedSkillId ? "isOpen" : ""}`} aria-label={t("libraryRoute.skillPreview")} aria-hidden={!selectedSkillId}>
          <header><button className="quietCloseButton" type="button" aria-label={t("libraryRoute.closeSkillPreview")} onClick={() => setSelectedSkillId("")}><X aria-hidden="true" /></button><span>Skill</span></header>
          {skillDetailLoading ? <div className="librarySkillPeekState" role="status" aria-live="polite"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryRoute.openingSkill")}</div> : null}
          {skillDetailError ? <div className="errorBanner libraryError" role="alert">{skillDetailError}</div> : null}
          {skillDetail ? <div className="librarySkillPeekScroll">
            <div className="librarySkillBanner"><Layers aria-hidden="true" />{t("libraryRoute.thisFileIsMountedAsASkillForThe")}</div>
            <section className="librarySkillSummary">
              <span className="librarySkillIcon"><Layers aria-hidden="true" /></span>
              <h2>{skillDetail.skill.name}</h2>
              <p>{skillDetail.skill.description}</p>
              <dl>
                <div><dt>{t("libraryRoute.source")}</dt><dd>{skillDetail.skill.skillId.startsWith("system:") ? t("libraryRoute.systemMount") : t("libraryRoute.pluginMount")}</dd></div>
                <div><dt>{t("libraryRoute.invocation")}</dt><dd>{skillDetail.skill.allowImplicitInvocation ? t("libraryRoute.automaticOrExplicit") : t("libraryRoute.explicitOnly")}</dd></div>
                <div><dt>{t("libraryRoute.allowedTools")}</dt><dd>{skillDetail.skill.allowedTools.length ? skillDetail.skill.allowedTools.join(" · ") : t("libraryRoute.unrestricted")}</dd></div>
              </dl>
            </section>
            <article className="librarySkillDocument"><SkillMarkdown content={skillDetail.content} /></article>
          </div> : null}
        </aside>
      </div>

      {dialog && <div className="libraryDialogBackdrop" role="presentation" onMouseDown={closeDialog}>
        <section className={`libraryDialog ${dialog === "move" ? "libraryMoveDialog" : ""} ${dialog === "upload" ? "libraryUploadDialog" : ""}`} ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="library-dialog-title" tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
          {dialog === "upload" && <>
            <header className="libraryUploadHeader"><div><h2 id="library-dialog-title">{t("libraryRoute.uploadFiles")}</h2><p>{t("library.destination", { destination: folderPath.length ? `“${folderPath.at(-1).displayName}”` : t("libraryRoute.myMaterials") })}</p></div><span>{uploadQueue.length}/{MAX_UPLOAD_BATCH_FILES}</span></header>
            <div
              className={`libraryUploadDropzone ${uploadDragActive ? "is-dragging" : ""}`}
              role="button"
              tabIndex={uploading ? -1 : 0}
              aria-disabled={uploading}
              onClick={() => !uploading && fileInputRef.current?.click()}
              onKeyDown={(event) => { if (!uploading && (event.key === "Enter" || event.key === " ")) { event.preventDefault(); fileInputRef.current?.click(); } }}
              onDragEnter={(event) => { event.preventDefault(); if (!uploading) setUploadDragActive(true); }}
              onDragOver={(event) => { event.preventDefault(); if (!uploading) { event.dataTransfer.dropEffect = "copy"; setUploadDragActive(true); } }}
              onDragLeave={(event) => { if (!event.currentTarget.contains(event.relatedTarget)) setUploadDragActive(false); }}
              onDrop={(event) => { event.preventDefault(); setUploadDragActive(false); addUploadFiles(event.dataTransfer.files); }}
            >
              <span className="libraryUploadDropIcon"><Upload aria-hidden="true" /></span>
              <strong>{uploadDragActive ? t("libraryRoute.dropToAddToTheQueue") : t("libraryRoute.dragOneOrMoreFilesHere")}</strong>
              <p>{t("libraryRoute.orClickHereToChooseOneFileAtA")}</p>
            </div>
            <input ref={fileInputRef} className="srOnly" type="file" aria-label={t("libraryRoute.chooseAFile")} disabled={uploading} onChange={selectUploadFile} />
            {uploadDialogError && <div className="libraryUploadError" role="alert">{uploadDialogError}</div>}
            {uploadQueue.length ? <div className="libraryUploadQueue" aria-label={t("libraryRoute.uploadQueue")}>{uploadQueue.map((file) => <div className="libraryUploadQueueItem" key={uploadFileKey(file)}><span><FileText aria-hidden="true" /></span><div><strong>{file.name}</strong><small>{formatBytes(file.size)}</small></div><button type="button" disabled={uploading} onClick={() => removeUploadFile(file)} aria-label={t("attachmentCard.removeValue", { value1: file.name })}><X aria-hidden="true" /></button></div>)}</div> : null}
            <div className="libraryDialogActions libraryUploadActions"><button type="button" disabled={uploading} onClick={closeUploadDialog}>{t("agentRunRow.cancel")}</button><button className="primary" type="button" disabled={uploading || !uploadQueue.length} aria-live="polite" onClick={uploadQueuedFiles}>{uploading ? <><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryRoute.uploading")}</> : t("libraryRoute.uploadValueFiles", { value1: uploadQueue.length })}</button></div>
          </>}
          {dialog === "folder" && <><h2 id="library-dialog-title">{t("libraryRoute.newFolder")}</h2><input autoFocus value={dialogTitle} onChange={(event) => setDialogTitle(event.target.value)} placeholder={t("libraryRoute.folderName")} /><div className="libraryDialogActions"><button type="button" onClick={() => setDialog("")}>{t("agentRunRow.cancel")}</button><button className="primary" type="button" disabled={working || !dialogTitle.trim()} onClick={createFolder}>{t("libraryRoute.create")}</button></div></>}
          {dialog === "move" && <><header className="libraryMoveHeader"><h2 id="library-dialog-title">{t("libraryRoute.moveTo")}</h2><button type="button" onClick={() => setDialog("")} aria-label={t("libraryRoute.closeMoveDialog")}><X aria-hidden="true" /></button></header><button className="libraryMoveRoot" type="button" onClick={() => loadMoveDirectory("")}>{t("workspaceContextPanel.library")}</button>{error && <div className="errorBanner" role="alert">{error}</div>}<div className="libraryMoveObjects">{moveLoading ? <div className="libraryEmptyState" role="status" aria-live="polite"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("libraryRoute.loadingFolders")}</div> : moveObjects.map((item) => { const ItemIcon = isFolder(item) ? Folder : isImage(item) ? ImageIcon : FileText; const disabled = !isFolder(item) || selectedIds.includes(item.id); return isFolder(item) ? <button className="libraryMoveObject folder" type="button" key={item.id} disabled={disabled} onClick={() => loadMoveDirectory(item.id)}><ItemIcon aria-hidden="true" /><span>{item.displayName}</span></button> : <div className="libraryMoveObject file" key={item.id}><ItemIcon aria-hidden="true" /><span>{item.displayName}</span></div>; })}</div><footer className="libraryMoveActions">{creatingMoveFolder ? <form className="libraryMoveCreate" onSubmit={(event) => { event.preventDefault(); createMoveFolder(); }}><input autoFocus value={moveFolderName} onChange={(event) => setMoveFolderName(event.target.value)} placeholder={t("libraryRoute.newFolderName")} /><button className="primary" type="submit" disabled={working || !moveFolderName.trim()}>{t("libraryRoute.create")}</button></form> : <button type="button" disabled={working} onClick={() => { setError(""); setCreatingMoveFolder(true); }}>{t("libraryRoute.newFolder")}</button>}<span /><button type="button" onClick={() => setDialog("")}>{t("agentRunRow.cancel")}</button><button className="primary" type="button" disabled={working || moveLoading} onClick={moveSelected}>{t("libraryRoute.moveHere")}</button></footer></>}
          {dialog === "agent" && <><h2 id="library-dialog-title">{t("libraryRoute.selectAgent")}</h2><div className="libraryAgentChoices">{agents.map((agent) => <button type="button" key={agent.id} onClick={() => void createChat(agent.id)}>{agent.name}</button>)}</div><div className="libraryDialogActions"><button type="button" onClick={() => setDialog("")}>{t("agentRunRow.cancel")}</button></div></>}
        </section>
      </div>}
    </ShellPage>
  );
}

export default function LibraryPage() {
  useTranslation();
  return <LibraryPageContent />;
}
