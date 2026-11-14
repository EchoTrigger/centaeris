import { FileCheck2, FileText, MessageSquare, X } from "lucide-react";
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { useSearchHistory } from "./workspaceSearchState";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";
import { sessionChatPath } from "./sessionNavigation.mjs";

export function useWorkspaceSearch(_navigationKey, enabled = true) {
  const location = useLocation();
  const navigate = useNavigate();
  const triggerRef = useRef(null);
  const openerRef = useRef(null);
  const inputRef = useRef(null);
  const open = enabled && (location.hash === "#search" || location.hash.startsWith("#search?"));

  const close = useCallback(() => {
    if (location.state?.workspaceSearchOpened) navigate(-1);
    else navigate({ ...location, hash: "" }, { replace: true });
    window.requestAnimationFrame(() => {
      const previous = openerRef.current;
      const opener = previous?.isConnected && previous.getClientRects().length ? previous : triggerRef.current;
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    });
  }, [location, navigate]);

  const show = useCallback(() => {
    openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    navigate({ ...location, hash: "#search" }, { state: { ...location.state, workspaceSearchOpened: true } });
  }, [location, navigate]);

  useEffect(() => {
    if (!enabled) return undefined;
    function handleShortcut(event) {
      if (event.defaultPrevented) return;
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        if (open) inputRef.current?.focus();
        else show();
      } else if (open && event.key === "Escape") {
        if (event.isComposing) return;
        event.preventDefault();
        close();
      }
    }
    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [enabled, open, show, close]);

  return useMemo(() => ({ open, close, triggerRef, inputRef, toggle: open ? close : show }), [open, close, show]);
}

function SearchText({ text, query }) {
  const parts = [];
  const folded = text.toLocaleLowerCase();
  let start = 0;
  let position = folded.indexOf(query);
  while (query && position >= 0) {
    parts.push(text.slice(start, position), <mark key={position}>{text.slice(position, position + query.length)}</mark>);
    start = position + query.length;
    position = folded.indexOf(query, start);
  }
  parts.push(text.slice(start));
  return parts;
}

function useAuthorizedSearch({ endpoint, query, enabled, revision, restored }) {
  const selection = JSON.stringify([endpoint, query]);
  const key = JSON.stringify([selection, revision]);
  const [response, setResponse] = useState(null);
  const request = useRef(null);
  const pages = useRef({ selection, cursors: restored?.selection === selection ? restored.cursors : [] });
  const result = response?.key === key ? response : null;
  useEffect(() => {
    if (!query || !enabled) return;
    if (pages.current.selection !== selection) pages.current = { selection, cursors: [] };
    const controller = new AbortController();
    request.current = controller;
    const getPage = async cursor => {
      const value = await apiJson(`${endpoint}?${new URLSearchParams({ query, ...(cursor ? { cursor } : {}) })}`, { signal: controller.signal });
      if (!Array.isArray(value.matches) || (value.nextCursor !== null && typeof value.nextCursor !== "string")) throw new Error("workspace_search_response_invalid");
      return value;
    };
    const timer = window.setTimeout(async () => {
      try {
        let value = await getPage();
        const loaded = [];
        // Reauthorize restored pages before displaying their content. Keep only
        // navigation preferences in history, never authoritative search results.
        for (const cursor of pages.current.cursors) {
          if (value.nextCursor !== cursor) break;
          const next = await getPage(cursor);
          value = { ...next, matches: [...value.matches, ...next.matches] };
          loaded.push(cursor);
        }
        if (!controller.signal.aborted) {
          pages.current = { selection, cursors: loaded };
          setResponse({ key, ...value, failed: false, loadingMore: false });
        }
      } catch {
        if (!controller.signal.aborted) setResponse({ key, matches: [], nextCursor: null, failed: true, loadingMore: false });
      }
    }, 250);
    return () => { window.clearTimeout(timer); controller.abort(); request.current?.abort(); };
  }, [endpoint, query, enabled, selection, key]);
  async function loadMore() {
    if (!result?.nextCursor || result.loadingMore) return;
    const controller = new AbortController(); request.current = controller;
    const cursor = result.nextCursor;
    setResponse(current => ({ ...current, loadingMore: true }));
    try {
      const value = await apiJson(`${endpoint}?${new URLSearchParams({ query, cursor })}`, { signal: controller.signal });
      if (!Array.isArray(value.matches) || (value.nextCursor !== null && typeof value.nextCursor !== "string")) throw new Error("workspace_search_response_invalid");
      if (!controller.signal.aborted) {
        pages.current = { selection, cursors: [...pages.current.cursors, cursor] };
        setResponse(current => {
          const ids = new Set(current.matches.map(item => item.id ?? item.sessionId));
          return { key, ...value, matches: [...current.matches, ...value.matches.filter(item => !ids.has(item.id ?? item.sessionId))], failed: false, loadingMore: false };
        });
      }
    } catch { if (!controller.signal.aborted) setResponse(current => ({ ...current, failed: true, loadingMore: false })); }
  }
  return { result, loadMore, pages };
}

export function WorkspaceSearch({ workspaceId, onClose, inputRef }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const location = useLocation();
  const history = useSearchHistory();
  const restored = useRef(history?.get(location.key));
  const params = new URLSearchParams(location.hash.startsWith("#search?") ? location.hash.slice(8) : "");
  const query = params.get("q") || "";
  const filter = ["conversations", "library"].includes(params.get("filter")) ? params.get("filter") : "all";
  function updateQuery(value, nextFilter = filter) {
    const next = new URLSearchParams({ q: value, filter: nextFilter });
    navigate({ ...location, hash: `#search?${next}` }, { replace: true, state: location.state });
  }
  const [attempt, setAttempt] = useState(restored.current?.attempt ?? 0);
  const [focusRevision, setFocusRevision] = useState(0);
  const [selectedIndex, setSelectedIndex] = useState(restored.current?.selectedIndex ?? 0);
  const resultsRef = useRef(null);
  const restoreScroll = useRef(restored.current?.scrollTop ?? null);
  const listId = useId();
  const text = query.trim().toLocaleLowerCase();
  const revision = `${attempt}:${focusRevision}`;
  const resourceSearch = useAuthorizedSearch({ endpoint: `/api/workspaces/${encodeURIComponent(workspaceId)}/search`, query: text,
    enabled: filter !== "conversations", revision, restored: restored.current?.resourcePages });
  const sessionSearch = useAuthorizedSearch({ endpoint: `/api/workspaces/${encodeURIComponent(workspaceId)}/sessions/search`, query: text,
    enabled: filter !== "library", revision, restored: restored.current?.sessionPages });
  const resources = resourceSearch.result;
  const conversations = sessionSearch.result;
  useEffect(() => { inputRef?.current?.focus({ preventScroll: true }); }, [inputRef]);
  useEffect(() => {
    const refresh = () => setFocusRevision(value => value + 1);
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, []);
  const groups = [
    { type: "library", label: t("searchOverlay.notesAndMaterials"), icon: FileText,
      results: (resources?.matches || []).filter(item => item.kind === "material") },
    { type: "library", label: t("searchOverlay.deliverables"), icon: FileCheck2,
      results: (resources?.matches || []).filter(item => item.kind === "deliverable") },
    { type: "conversations", label: t("searchOverlay.conversations"), icon: MessageSquare,
      results: (conversations?.matches || []).map(session => ({ title: session.title || t("searchOverlay.untitledConversation"),
        snippet: session.match.snippet, kind: session.match.kind, href: sessionChatPath(workspaceId, session.sessionId)
          + (session.match.eventId ? `?${new URLSearchParams({ at: session.match.eventId, highlight: text })}` : "") })) },
  ].filter(group => text && (filter === "all" || filter === group.type));
  const results = groups.flatMap(group => group.results);
  const activeIndex = Math.min(selectedIndex, results.length - 1);
  const optionId = index => `${listId}-${index}`;
  const loading = text && ((filter !== "conversations" && (resources === null || resources.loadingMore))
    || (filter !== "library" && (conversations === null || conversations.loadingMore)));
  const libraryFailed = filter !== "conversations" && resources?.failed;
  const conversationsFailed = filter !== "library" && conversations?.failed;
  const failed = libraryFailed || conversationsFailed;
  useLayoutEffect(() => {
    if (restoreScroll.current !== null && results.length && resultsRef.current) {
      resultsRef.current.scrollTop = restoreScroll.current; restoreScroll.current = null;
    }
  }, [results.length]);
  function select(href) {
    history?.set(location.key, { attempt, selectedIndex, scrollTop: resultsRef.current?.scrollTop ?? 0,
      resourcePages: resourceSearch.pages.current, sessionPages: sessionSearch.pages.current });
    navigate(href);
  }
  function handleKeyDown(event) {
    if (event.nativeEvent.isComposing || !results.length) return;
    if (event.key === "Enter") { event.preventDefault(); select(results[activeIndex].href); }
    else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const next = (activeIndex + (event.key === "ArrowDown" ? 1 : -1) + results.length) % results.length;
      setSelectedIndex(next); document.getElementById(optionId(next))?.scrollIntoView?.({ block: "nearest" });
    }
  }
  return <section className="workspaceSearchPanel" role="search" aria-label={t("searchOverlay.workspaceSearch")}>
    <header className="shSearchInputRow">
      <input ref={inputRef} autoFocus role="combobox" aria-expanded={Boolean(results.length)} aria-controls={listId}
        aria-autocomplete="list" aria-activedescendant={activeIndex >= 0 ? optionId(activeIndex) : undefined}
        value={query} maxLength={256} onChange={event => { updateQuery(event.target.value); setSelectedIndex(0); }}
        onKeyDown={handleKeyDown} placeholder={t("searchOverlay.searchWorkspace")} aria-label={t("searchOverlay.searchConversationsAndNotes")} />
      <button className="workspacePanelToggle" type="button" aria-label={t("workspaceContextPanel.close")} onClick={onClose}><X aria-hidden="true" /></button>
    </header>
    <div className="shSearchFilters" role="group" aria-label={t("searchOverlay.searchResults")}>
      {[["all", t("libraryRoute.all")], ["conversations", t("searchOverlay.conversations")], ["library", t("libraryObjectRoute.library")]].map(([key, label]) => (
        <button type="button" aria-pressed={key === filter} key={key} onClick={() => { updateQuery(query, key); setSelectedIndex(0); }}>{label}</button>
      ))}
      {text && !results.length && !failed && !loading ? <small role="status">{t("common.resultCount", { count: results.length })}</small> : null}
    </div>
    <div className="shSearchResults" ref={resultsRef} id={listId} role="listbox" aria-label={t("searchOverlay.searchResults")}>
      {groups.map(({ label, icon: Icon, results: groupResults }) => groupResults.length ? <div key={label} role="group" aria-label={label}>
        <div className="shSearchGroupLabel" aria-hidden="true">{label}</div>
        {groupResults.map(result => {
          const index = results.indexOf(result);
          return <button id={optionId(index)} type="button" role="option" tabIndex={-1} aria-selected={index === activeIndex} key={result.href}
            onMouseEnter={() => setSelectedIndex(index)} onMouseDown={event => event.preventDefault()} onClick={() => select(result.href)}>
            <Icon aria-hidden="true" /><span className="shSearchResultText"><strong><SearchText text={result.title} query={text} /></strong>
              {result.snippet ? <small><span>{t(`searchOverlay.match.${result.kind}`)}</span><SearchText text={result.snippet} query={text} /></small> : null}
            </span>
          </button>;
        })}
      </div> : null)}
    </div>
    {text && filter !== "conversations" && resources?.nextCursor ? <div className="shSearchNotice"><button type="button" disabled={resources.loadingMore} onClick={() => void resourceSearch.loadMore()}>{t("searchOverlay.loadMoreResources")}</button></div> : null}
    {text && filter !== "library" && conversations?.nextCursor ? <div className="shSearchNotice"><button type="button" disabled={conversations.loadingMore} onClick={() => void sessionSearch.loadMore()}>{t("searchOverlay.loadMoreConversations")}</button></div> : null}
    {text && failed ? <div className="shSearchNotice"><span role="alert">{t(libraryFailed && conversationsFailed ? "searchOverlay.loadFailed" : libraryFailed ? "searchOverlay.libraryLoadFailed" : "searchOverlay.conversationsLoadFailed")}</span>
      <button type="button" onClick={() => { setAttempt(value => value + 1); inputRef?.current?.focus(); }}>{t("agentChat.retry")}</button>
    </div> : null}
    {text && loading ? <p className="shSearchNotice" role="status">{t("agentChat.loading")}</p> : null}
  </section>;
}
