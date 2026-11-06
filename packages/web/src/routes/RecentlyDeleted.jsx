
import { useTranslation } from "../i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, LoaderCircle, RotateCcw, Search, Trash2 } from "lucide-react";
import { apiJson } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";

function useTrashFeed(path, field, enabled) {
  useTranslation();
  const [items, setItems] = useState([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");
  const [filterOptions, setFilterOptions] = useState({ deletedBy: [], locations: [] });
  const loadingRef = useRef(false);
  const cursorRef = useRef(null);

  const load = useCallback(async (nextCursor = null, replace = false) => {
    if (!enabled || loadingRef.current || (!replace && nextCursor !== cursorRef.current)) return;
    loadingRef.current = true;
    setLoading(true);
    setError("");
    try {
      const separator = path.includes("?") ? "&" : "?";
      const result = await apiJson(`${path}${nextCursor ? `${separator}cursor=${encodeURIComponent(nextCursor)}` : ""}`);
      if (!Array.isArray(result[field]) || typeof result.hasMore !== "boolean" || result.hasMore !== Boolean(result.nextCursor)) throw new Error("trash_page_invalid");
      setItems((current) => replace ? result[field] : [...current, ...result[field]]);
      if (result.filterOptions) setFilterOptions(result.filterOptions);
      cursorRef.current = result.nextCursor;
      setHasMore(result.hasMore);
      setLoaded(true);
    } catch {
      setError(true);
    } finally {
      loadingRef.current = false;
      setLoading(false);
    }
  }, [enabled, field, path]);

  useEffect(() => {
    if (!enabled) return;
    cursorRef.current = null;
    setItems([]);
    setHasMore(false);
    setLoaded(false);
    setError("");
    void load(null, true);
  }, [enabled, load]);

  return {
    items, loaded, loading, error, hasMore, filterOptions,
    remove: (id) => setItems((current) => current.filter((item) => item.id !== id)),
    reload: () => load(null, true),
    retry: () => load(cursorRef.current, !cursorRef.current),
    loadMore: () => cursorRef.current && load(cursorRef.current),
  };
}

function LazySentinel({ feed }) {
  const { t } = useTranslation();
  const ref = useRef(null);
  useEffect(() => {
    if (!ref.current || !feed.hasMore) return undefined;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) feed.loadMore();
    }, { rootMargin: "120px" });
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, [feed.hasMore, feed.loadMore]);
  if (!feed.hasMore) return null;
  return <div className="shTrashSentinel" ref={ref}>{feed.loading ? <LoaderCircle className="statusIcon" aria-label={t("trashPopover.loading")} /> : null}</div>;
}

function RemainingDays({ deletedAt }) {
  const { t } = useTranslation();
  const deleted = new Date(deletedAt).valueOf();
  if (!Number.isFinite(deleted)) return <span>{t("trashPopover.deletionTimeUnknown")}</span>;
  const days = Math.max(1, Math.ceil((deleted + 30 * 86400000 - Date.now()) / 86400000));
  return <span>{t("trash.days", { count: days })}</span>;
}

function TrashMetadata({ item }) {
  const { t } = useTranslation();
  return <small>
    {item.location?.label || t("trashPopover.originalLocationUnknown")} · {item.deletedBy?.email || t("trashPopover.deletedByUnknownUser")} · <RemainingDays deletedAt={item.deletedAt} />
  </small>;
}

function RowActions({ label, busy, onRestore, onPurge }) {
  const { t } = useTranslation();
  return <span className="shTrashActions">
    <button type="button" disabled={busy} aria-label={t("trashPopover.restoreValue", { value1: label })} title={t("trashPopover.restore")} onClick={onRestore}><RotateCcw aria-hidden="true" /></button>
    <button type="button" disabled={busy} aria-label={t("trashPopover.permanentlyDeleteValue", { value1: label })} title={t("trashSessionRoute.deletePermanently")} onClick={onPurge}><Trash2 aria-hidden="true" /></button>
  </span>;
}

function TrashSection({ feed, children }) {
  const { t } = useTranslation();
  if (feed.loaded && !feed.items.length && !feed.error) return null;
  return <section className="shTrashSection">
    <div className="shTrashSectionBody">
      {feed.items.map(children)}
      {feed.error ? <button className="shTrashRetry" type="button" onClick={feed.retry}>{t("trashPopover.unableToLoadTryAgain")}</button> : null}
      <LazySentinel feed={feed} />
    </div>
  </section>;
}

function TrashItemRow({ item, busy, error, onRestore, onPurge }) {
  return <div className="shTrashRow">
    <div className="shTrashOpenRow"><span><strong>{item.title}</strong><TrashMetadata item={item} />{error ? <small className="error" role="alert">{error}</small> : null}</span></div>
    <RowActions label={item.title} busy={busy} onRestore={() => onRestore(item)} onPurge={() => onPurge(item)} />
  </div>;
}

export function RecentlyDeleted({ workspace }) {
  const { t } = useTranslation();
  const [searchDraft, setSearchDraft] = useState("");
  const [search, setSearch] = useState("");
  const [location, setLocation] = useState("");
  const [deletedByUserId, setDeletedByUserId] = useState("");
  const trashPath = useMemo(() => {
    const params = new URLSearchParams({ kind: "library" });
    if (search) params.set("query", search);
    if (location) {
      const [locationKind, locationId = ""] = location.split("|");
      params.set("locationKind", locationKind);
      if (locationId) params.set("locationId", locationId);
    }
    if (deletedByUserId) params.set("deletedByUserId", deletedByUserId);
    const query = params.toString();
    return `/api/workspaces/${encodeURIComponent(workspace.id)}/trash${query ? `?${query}` : ""}`;
  }, [deletedByUserId, location, search, workspace.id]);
  const trash = useTrashFeed(trashPath, "items", true);
  const [busyKey, setBusyKey] = useState("");
  const [actionError, setActionError] = useState(null);
  const [purgeTarget, setPurgeTarget] = useState(null);

  useEffect(() => {
    const timer = window.setTimeout(() => setSearch(searchDraft.trim()), 180);
    return () => window.clearTimeout(timer);
  }, [searchDraft]);

  async function restore(item) {
    if (busyKey) return;
    const key = item.id;
    setBusyKey(key);
    setActionError(null);
    try {
      await apiJson("/api/library/" + encodeURIComponent(item.id) + "/restore", { method: "POST" });
      trash.remove(item.id);
    } catch (requestError) {
      if (requestError.status === 404 || requestError.status === 410 || requestError.message.endsWith("_not_deleted")) await trash.reload();
      else setActionError({ key, message: t("trashSessionRoute.unableToRestore") });
    } finally {
      setBusyKey("");
    }
  }

  async function purge(target = purgeTarget) {
    if (!target || busyKey) return;
    const key = target.id;
    setBusyKey(key);
    setActionError(null);
    try {
      await apiJson("/api/library/" + encodeURIComponent(key) + "/trash", { method: "DELETE" });
      trash.remove(key);
      setPurgeTarget(null);
    } catch (requestError) {
      if (requestError.status === 404 || requestError.status === 410 || requestError.message.endsWith("_not_deleted")) {
        await trash.reload();
        setPurgeTarget(null);
      } else setActionError({ key, message: t("trashSessionRoute.unableToDeletePermanently") });
    } finally {
      setBusyKey("");
    }
  }

  const allLoaded = trash.loaded || trash.error;
  const empty = allLoaded && !trash.items.length && !trash.error;
  return <section className="shTrashPage" aria-label={t("settingsRoute.recentlyDeleted")}>
      <div className="shTrashTools">
        <label className="shTrashSearch"><Search aria-hidden="true" /><input aria-label={t("trashPopover.searchTrash")} placeholder={t("trashPopover.searchInTrash")} value={searchDraft} onChange={(event) => setSearchDraft(event.target.value)} /></label>
        <div className="shTrashFilters">
          <label data-active={Boolean(location)}><span>{t("trashPopover.originalLocation")}</span><select aria-label={t("trashPopover.originalLocation")} value={location} onChange={(event) => setLocation(event.target.value)}>
            <option value="">{t("libraryRoute.all")}</option>
            {trash.filterOptions.locations.filter(item => item.scope === "privateLibrary").map(item =>
              <option key={item.id || item.kind} value={item.kind + "|" + (item.id || "")}>{item.label}</option>)}
          </select><ChevronDown aria-hidden="true" /></label>
          <label data-active={Boolean(deletedByUserId)}><span>{t("trashPopover.deletedBy")}</span><select aria-label={t("trashPopover.deletedBy")} value={deletedByUserId} onChange={(event) => setDeletedByUserId(event.target.value)}>
            <option value="">{t("libraryRoute.all")}</option>
            {trash.filterOptions.deletedBy.map((item) => <option key={item.userId} value={item.userId}>{item.email}</option>)}
          </select><ChevronDown aria-hidden="true" /></label>
        </div>
      </div>
      <div className="shTrashPopoverBody">
        {!allLoaded ? <div className="shTrashLoading"><LoaderCircle className="statusIcon" aria-hidden="true" />{t("trashPopover.loading2")}</div> : null}
        <TrashSection feed={trash}>{(item) => {
          return <TrashItemRow key={item.id} item={item} busy={Boolean(busyKey)} error={!purgeTarget && actionError?.key === item.id ? actionError.message : ""} onRestore={restore} onPurge={target => { setActionError(null); setPurgeTarget(target); }} />;
        }}</TrashSection>
        {empty ? <div className="shTrashAllEmpty" role="status"><Trash2 aria-hidden="true" /><p>{t(search || location || deletedByUserId ? "trashPopover.noMatchingItems" : "trashPopover.trashIsEmpty")}</p></div> : null}
      </div>
      <footer>{t("trashPopover.itemsStayInTrashFor30DaysBeforeBeing")}</footer>
      <ConfirmDialog open={Boolean(purgeTarget)} title={t("trashPopover.deleteThisMaterial")} message={actionError?.message} busy={Boolean(busyKey)} onCancel={() => setPurgeTarget(null)} onConfirm={() => void purge()} />
  </section>;
}
