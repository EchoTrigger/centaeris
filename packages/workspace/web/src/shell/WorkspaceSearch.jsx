import { FileText, MessageSquare, X } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { apiJson } from "../api";
import { useTranslation } from "../i18n";
import { sessionChatPath } from "./sessionNavigation.mjs";

export function useWorkspaceSearch(navigationKey, enabled = true) {
  const key = navigationKey ?? "";
  const [openFor, setOpenFor] = useState(null);
  const triggerRef = useRef(null);
  const openerRef = useRef(null);
  const inputRef = useRef(null);
  const open = enabled && openFor === key;
  if (openFor !== null && !open) setOpenFor(null);

  const close = useCallback(() => {
    setOpenFor(null);
    window.requestAnimationFrame(() => {
      const previous = openerRef.current;
      const opener = previous?.isConnected && previous.getClientRects().length ? previous : triggerRef.current;
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    });
  }, []);

  const show = useCallback(() => {
    openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setOpenFor(key);
  }, [key]);

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

export function WorkspaceSearch({ sessions, workspaceId, onClose, inputRef }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [libraryResponse, setLibraryResponse] = useState(null);
  const [sessionResponse, setSessionResponse] = useState(null);
  const [attempt, setAttempt] = useState(0);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const listId = useId();
  const base = `/w/${encodeURIComponent(workspaceId)}`;
  const needsSessions = !sessions?.length;
  const library = libraryResponse?.attempt === attempt ? libraryResponse.items : null;
  const fetchedSessions = sessionResponse?.attempt === attempt ? sessionResponse.items : null;

  useEffect(() => { inputRef?.current?.focus({ preventScroll: true }); }, [inputRef]);

  useEffect(() => {
    const controller = new AbortController();
    apiJson("/api/library", { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setLibraryResponse({ attempt, items: result.objects || [] }); })
      .catch(() => { if (!controller.signal.aborted) setLibraryResponse({ attempt, items: "failed" }); });
    return () => controller.abort();
  }, [attempt]);

  useEffect(() => {
    if (!needsSessions) return undefined;
    const controller = new AbortController();
    apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/sessions`, { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setSessionResponse({ attempt, items: result.sessions || [] }); })
      .catch(() => { if (!controller.signal.aborted) setSessionResponse({ attempt, items: "failed" }); });
    return () => controller.abort();
  }, [attempt, needsSessions, workspaceId]);

  const text = query.trim().toLocaleLowerCase();
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
      results: (needsSessions ? Array.isArray(fetchedSessions) ? fetchedSessions : [] : sessions).filter(session => matches(session.title))
        .slice(0, 12).map(session => ({ title: session.title || t("searchOverlay.untitledConversation"), href: sessionChatPath(workspaceId, session.id) })),
    },
  ].filter(group => text && (filter === "all" || filter === group.type));
  const results = groups.flatMap(group => group.results);
  const activeIndex = Math.min(selectedIndex, results.length - 1);
  const optionId = index => `${listId}-${index}`;
  const loading = (filter !== "conversations" && library === null)
    || (filter !== "library" && needsSessions && fetchedSessions === null);
  const libraryFailed = filter !== "conversations" && library === "failed";
  const conversationsFailed = filter !== "library" && needsSessions && fetchedSessions === "failed";
  const failed = libraryFailed || conversationsFailed;

  function select(href) {
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
        value={query} onChange={event => { setQuery(event.target.value); setSelectedIndex(0); }}
        onKeyDown={handleKeyDown} placeholder={t("searchOverlay.searchWorkspace")} aria-label={t("searchOverlay.searchConversationsAndNotes")} />
      <button className="workspacePanelToggle" type="button" aria-label={t("workspaceContextPanel.close")} onClick={onClose}><X aria-hidden="true" /></button>
    </header>
    <div className="shSearchFilters" role="group" aria-label={t("searchOverlay.searchResults")}>
      {[["all", t("libraryRoute.all")], ["conversations", t("searchOverlay.conversations")], ["library", t("libraryObjectRoute.library")]].map(([key, label]) => (
        <button type="button" aria-pressed={key === filter} key={key} onClick={() => { setFilter(key); setSelectedIndex(0); }}>{label}</button>
      ))}
    </div>
    <div className="shSearchResults" id={listId} role="listbox" aria-label={t("searchOverlay.searchResults")}>
      {groups.map(({ label, icon: Icon, results: groupResults }) => groupResults.length ? (
        <div key={label} role="group" aria-label={label}>
          <div className="shSearchGroupLabel" aria-hidden="true">{label}</div>
          {groupResults.map(result => {
            const index = results.indexOf(result);
            return <button id={optionId(index)} type="button" role="option" tabIndex={-1}
              aria-selected={index === activeIndex} key={result.href}
              onMouseEnter={() => setSelectedIndex(index)} onMouseDown={event => event.preventDefault()} onClick={() => select(result.href)}>
              <Icon aria-hidden="true" />
              <span>{result.title}</span>
            </button>;
          })}
        </div>
      ) : null)}
    </div>
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
