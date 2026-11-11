import { FileText, MessageSquare, X } from "lucide-react";
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
  const [libraryResponse, setLibraryResponse] = useState(restored.current?.libraryResponse ?? null);
  const [sessionResponse, setSessionResponse] = useState(restored.current?.sessionResponse ?? null);
  const [attempt, setAttempt] = useState(0);
  const [selectedIndex, setSelectedIndex] = useState(restored.current?.selectedIndex ?? 0);
  const resultsRef = useRef(null);
  const restoreScroll = useRef(restored.current?.scrollTop ?? null);
  const moreRequest = useRef(null);
  const listId = useId();
  const base = `/w/${encodeURIComponent(workspaceId)}`;
  const text = query.trim().toLocaleLowerCase();
  const searchKey = JSON.stringify([workspaceId, text, attempt]);
  const library = libraryResponse?.attempt === attempt ? libraryResponse.items : null;
  const conversations = sessionResponse?.key === searchKey ? sessionResponse : null;
  const libraryReady = libraryResponse?.attempt === attempt;
  const sessionReady = sessionResponse?.key === searchKey;

  useEffect(() => { inputRef?.current?.focus({ preventScroll: true }); }, [inputRef]);

  useEffect(() => {
    if (libraryReady) return undefined;
    const controller = new AbortController();
    apiJson("/api/library", { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setLibraryResponse({ attempt, items: result.objects || [] }); })
      .catch(() => { if (!controller.signal.aborted) setLibraryResponse({ attempt, items: "failed" }); });
    return () => controller.abort();
  }, [attempt, libraryReady]);

  useEffect(() => {
    if (!text || filter === "library" || sessionReady) return undefined;
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/sessions/search?${new URLSearchParams({ query: text })}`, { signal: controller.signal })
        .then(result => {
          if (!Array.isArray(result.matches) || (result.nextCursor !== null && typeof result.nextCursor !== "string")) throw new Error("session_search_response_invalid");
          if (!controller.signal.aborted) setSessionResponse({ key: searchKey, ...result, failed: false, loadingMore: false });
        })
        .catch(() => { if (!controller.signal.aborted) setSessionResponse({ key: searchKey, matches: [], nextCursor: null, failed: true, loadingMore: false }); });
    }, 250);
    return () => { window.clearTimeout(timer); controller.abort(); moreRequest.current?.abort(); };
  }, [filter, searchKey, text, workspaceId, sessionReady]);

  async function loadMore() {
    if (!conversations?.nextCursor || conversations.loadingMore) return;
    const controller = new AbortController();
    moreRequest.current = controller;
    setSessionResponse(current => ({ ...current, loadingMore: true }));
    try {
      const result = await apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/sessions/search?${new URLSearchParams({ query: text, cursor: conversations.nextCursor })}`, { signal: controller.signal });
      if (!Array.isArray(result.matches) || (result.nextCursor !== null && typeof result.nextCursor !== "string")) throw new Error("session_search_response_invalid");
      if (!controller.signal.aborted) setSessionResponse(current => {
        const ids = new Set(current.matches.map(item => item.sessionId));
        return { key: searchKey, matches: [...current.matches, ...result.matches.filter(item => !ids.has(item.sessionId))], nextCursor: result.nextCursor, failed: false, loadingMore: false };
      });
    } catch {
      if (!controller.signal.aborted) setSessionResponse(current => ({ ...current, failed: true, loadingMore: false }));
    }
  }

  const matches = value => (value || "").toLocaleLowerCase().includes(text);
  const groups = [
    {
      type: "library",
      label: t("searchOverlay.notesAndMaterials"),
      icon: FileText,
      results: (Array.isArray(library) ? library : []).filter(item => item.objectKind !== "folder" && matches(item.displayName))
        .slice(0, 12).map(item => ({ title: item.displayName, href: `${base}/library/${encodeURIComponent(item.id)}` })),
    },
    {
      type: "conversations",
      label: t("searchOverlay.conversations"),
      icon: MessageSquare,
      results: (conversations?.matches || []).map(session => ({ title: session.title || t("searchOverlay.untitledConversation"),
        snippet: session.match.snippet, kind: session.match.kind, href: sessionChatPath(workspaceId, session.sessionId)
          + (session.match.eventId ? `?${new URLSearchParams({ at: session.match.eventId, highlight: text })}` : "") })),
    },
  ].filter(group => text && (filter === "all" || filter === group.type));
  const results = groups.flatMap(group => group.results);
  const activeIndex = Math.min(selectedIndex, results.length - 1);
  const optionId = index => `${listId}-${index}`;
  const loading = (filter !== "conversations" && library === null)
    || (filter !== "library" && text && (conversations === null || conversations.loadingMore));
  const libraryFailed = filter !== "conversations" && library === "failed";
  const conversationsFailed = filter !== "library" && conversations?.failed;
  const failed = libraryFailed || conversationsFailed;

  useLayoutEffect(() => {
    if (restoreScroll.current !== null && results.length && resultsRef.current) {
      resultsRef.current.scrollTop = restoreScroll.current;
      restoreScroll.current = null;
    }
  }, [results.length]);

  function select(href) {
    history?.set(location.key, { selectedIndex, scrollTop: resultsRef.current?.scrollTop ?? 0, libraryResponse, sessionResponse });
    navigate(href);
  }

  function handleKeyDown(event) {
    if (event.nativeEvent.isComposing || !results.length) return;
    if (event.key === "Enter") {
      event.preventDefault();
      select(results[activeIndex].href);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      const next = (activeIndex + step + results.length) % results.length;
      setSelectedIndex(next);
      document.getElementById(optionId(next))?.scrollIntoView?.({ block: "nearest" });
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
    </div>
    <div className="shSearchResults" ref={resultsRef} id={listId} role="listbox" aria-label={t("searchOverlay.searchResults")}>
      {groups.map(({ label, icon: Icon, results: groupResults }) => groupResults.length ? (
        <div key={label} role="group" aria-label={label}>
          <div className="shSearchGroupLabel" aria-hidden="true">{label}</div>
          {groupResults.map(result => {
            const index = results.indexOf(result);
            return <button id={optionId(index)} type="button" role="option" tabIndex={-1}
              aria-selected={index === activeIndex} key={result.href}
              onMouseEnter={() => setSelectedIndex(index)} onMouseDown={event => event.preventDefault()} onClick={() => select(result.href)}>
              <Icon aria-hidden="true" />
              <span className="shSearchResultText"><strong><SearchText text={result.title} query={text} /></strong>
                {result.snippet ? <small><span>{t(`searchOverlay.match.${result.kind}`)}</span><SearchText text={result.snippet} query={text} /></small> : null}
              </span>
            </button>;
          })}
        </div>
      ) : null)}
    </div>
    {text && filter !== "library" && conversations?.nextCursor ? <div className="shSearchNotice">
      <button type="button" disabled={conversations.loadingMore} onClick={() => void loadMore()}>{t("searchOverlay.loadMoreConversations")}</button>
    </div> : null}
    {text && failed ? <div className="shSearchNotice">
      <span role="alert">{t(libraryFailed && conversationsFailed ? "searchOverlay.loadFailed" : libraryFailed ? "searchOverlay.libraryLoadFailed" : "searchOverlay.conversationsLoadFailed")}</span>
      <button type="button" onClick={() => {
        setAttempt(value => value + 1);
        inputRef?.current?.focus();
      }}>{t("agentChat.retry")}</button>
    </div> : null}
    {text && loading ? <p className="shSearchNotice" role="status">{t("agentChat.loading")}</p> : null}
    {!text || (!results.length && !failed && !loading) ? <div className="shSearchEmpty" role="status">
      <strong>{t(text ? "searchOverlay.noMatchingResults" : "libraryRoute.search")}</strong>
      {!text ? <span>{t("searchOverlay.enterKeywordsToSearch")}</span> : null}
    </div> : null}
  </section>;
}
