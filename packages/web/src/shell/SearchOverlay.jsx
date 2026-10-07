import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router";
import { Bot, FileText, MessageSquare, Search } from "lucide-react";
import { apiJson } from "../api";
import { useModalDialog } from "../components/useModalDialog";

const KIND_LABEL = () => ({ note: t("libraryRoute.note"), file: t("libraryRoute.materials"), session: t("searchOverlay.conversations"), agent: t("agentRunRow.agent") });

export function SearchOverlay({ sessions, workspace, agents, agentId, onClose }) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [library, setLibrary] = useState([]);
  const [loadedSessions, setLoadedSessions] = useState(sessions);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [notePreview, setNotePreview] = useState("");
  const navigate = useNavigate();
  const dialogRef = useModalDialog({ onClose });

  useEffect(() => {
    let active = true;
    apiJson("/api/library")
      .then((result) => { if (active) setLibrary(result.objects || []); })
      .catch(() => { if (active) setLibrary([]); });
    if (!sessions.length && workspace.id && agentId) {
      apiJson(`/api/workspaces/${workspace.id}/sessions?agentId=${encodeURIComponent(agentId)}`)
        .then((result) => { if (active) setLoadedSessions(result.sessions || []); })
        .catch(() => { if (active) setLoadedSessions([]); });
    }
    return () => { active = false; };
  }, [sessions, workspace.id, agentId]);

  const groups = useMemo(() => {
    const text = query.trim().toLocaleLowerCase();
    const matches = (value) => !text || value.toLocaleLowerCase().includes(text);
    return [
      {
        label: t("searchOverlay.notesAndMaterials"),
        icon: FileText,
        results: library
          .filter((item) => item.objectKind !== "folder" && matches(item.displayName || ""))
          .slice(0, 12)
          .map((item) => ({
            key: `library:${item.id}`,
            id: item.id,
            kind: item.objectKind === "note" ? "note" : "file",
            title: item.displayName,
            detail: item.objectKind === "note" ? t("searchOverlay.markdownNote") : item.contentType || t("libraryRoute.materials"),
            href: `/w/${encodeURIComponent(workspace.id)}/library/${item.id}`,
          })),
      },
      {
        label: t("searchOverlay.conversations"),
        icon: MessageSquare,
        results: loadedSessions
          .filter((session) => matches(session.title || ""))
          .slice(0, 12)
          .map((session) => ({ key: `session:${session.id}`, id: session.id, kind: "session", title: session.title || t("searchOverlay.untitledConversation"), detail: t("agentRunRow.agentSessions"), href: `/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(session.agentId)}?sessionId=${encodeURIComponent(session.id)}` })),
      },
      {
        label: t("agentRunRow.agent"),
        icon: Bot,
        results: agents
          .filter((agent) => matches(`${agent.name} ${agent.description}`))
          .map((agent) => ({ key: `agent:${agent.id}`, id: agent.id, kind: "agent", title: agent.name, detail: agent.description, href: `/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(agent.id)}` })),
      },
    ];
  }, [agents, library, loadedSessions, query, workspace.id, t]);
  const results = groups.flatMap((group) => group.results.map((result) => ({ ...result, icon: group.icon })));
  const selected = results[Math.min(selectedIndex, Math.max(0, results.length - 1))];

  // biome-ignore lint/correctness/useExhaustiveDependencies: Search-source changes intentionally reset keyboard selection to the first result.
  useEffect(() => setSelectedIndex(0), [query, library, loadedSessions]);

  useEffect(() => {
    let active = true;
    setNotePreview("");
    if (selected?.kind === "note") {
      apiJson(`/api/library/${selected.id}/note`)
        .then((result) => { if (active) setNotePreview(result.markdown || ""); })
        .catch(() => { if (active) setNotePreview(""); });
    }
    return () => { active = false; };
  }, [selected?.id, selected?.kind]);

  function open(result = selected) {
    if (!result) return;
    onClose();
    navigate(result.href);
  }

  function handleKeyDown(event) {
    if (event.key === "ArrowDown" && results.length) {
      event.preventDefault();
      setSelectedIndex((index) => (index + 1) % results.length);
    }
    if (event.key === "ArrowUp" && results.length) {
      event.preventDefault();
      setSelectedIndex((index) => (index - 1 + results.length) % results.length);
    }
    if (event.key === "Enter") {
      event.preventDefault();
      open();
    }
  }

  return createPortal(
    <div className="shSearchBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="shSearchDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-label={t("searchOverlay.workspaceSearch")} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()} onKeyDown={handleKeyDown}>
        <div className="shSearchInputRow">
          <Search aria-hidden="true" />
          <input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t("searchOverlay.searchWorkspace")} aria-label={t("searchOverlay.searchConversationsAndNotes")} />
        </div>
        <div className="shSearchFilters"><span>{t("searchOverlay.searchesTitlesAndLoadedDescriptionsOnly")}</span><span>{t("searchOverlay.conversationsNotesAgents")}</span></div>
        <div className="shSearchBody">
          <div className="shSearchResults" role="listbox" aria-label={t("searchOverlay.searchResults")}>
            {groups.map(({ label, icon: Icon, results: groupResults }) => groupResults.length ? (
              <section key={label}>
                <header>{label}</header>
                {groupResults.map((result) => {
                  const index = results.findIndex((item) => item.key === result.key);
                  return (
                    <button className={index === selectedIndex ? "isSelected" : ""} type="button" role="option" aria-selected={index === selectedIndex} key={result.key} onMouseEnter={() => setSelectedIndex(index)} onFocus={() => setSelectedIndex(index)} onClick={() => open(result)}>
                      <Icon aria-hidden="true" />
                      <span><strong>{result.title}</strong><small>{result.detail}</small></span>
                    </button>
                  );
                })}
              </section>
            ) : null)}
            {!results.length ? <p className="shSearchEmpty">{t("searchOverlay.noMatchingResults")}</p> : null}
          </div>
          <aside className="shSearchPreview" aria-label={t("searchOverlay.searchResultPreview")}>
            {selected ? (
              <>
                <div className="shSearchPreviewCover" />
                <div className="shSearchPreviewContent">
                  <span className="shSearchPreviewKind">{KIND_LABEL()[selected.kind]}</span>
                  <h2>{selected.title}</h2>
                  {notePreview ? <pre>{notePreview}</pre> : <p>{selected.preview || selected.detail}</p>}
                </div>
              </>
            ) : <p className="shSearchEmpty">{t("searchOverlay.enterKeywordsToSearch")}</p>}
          </aside>
        </div>
        <footer className="shSearchFoot"><span>{t("searchOverlay.select")}</span><span>{t("searchOverlay.enterOpen")}</span><span>{t("searchOverlay.escClose")}</span></footer>
      </section>
    </div>,
    document.body,
  );
}
