import { useState } from "react";
import { createPortal } from "react-dom";
import { Popover } from "@centaeris/ui/primitives/popover";
import { Search, SquarePen, Plus, X } from "lucide-react";
import { useTranslation } from "../i18n";
import { useModalDialog } from "../components/useModalDialog";
import "./conversation-dock.css";

function ProjectCreateDialog({ onClose, onCreate }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dialogRef = useModalDialog({ onClose });

  async function submit(event) {
    event.preventDefault();
    const normalizedName = name.trim();
    if (!normalizedName || busy) return;
    setBusy(true);
    setError("");
    try {
      await onCreate(normalizedName);
      onClose();
    } catch {
      setError(t("shellSidebar.unableToCreateProjectPleaseTryAgain"));
      setBusy(false);
    }
  }

  return createPortal(<div className="shPrivateCreateBackdrop" role="presentation" onMouseDown={onClose}>
    <form className="shProjectCreateDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="shProjectCreateTitle" tabIndex={-1} onSubmit={submit} onMouseDown={(event) => event.stopPropagation()}>
      <header><button className="quietCloseButton" type="button" aria-label={t("shellSidebar.closeProjectCreation")} onClick={onClose}><X aria-hidden="true" /></button><h2 id="shProjectCreateTitle">{t("shellSidebar.createProject")}</h2></header>
      <label>
        <span>{t("shellSidebar.projectName")}</span>
        <input className="shProjectNameInput" autoFocus maxLength={100} value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      {error ? <p role="alert">{error}</p> : null}
      <footer><button type="button" onClick={onClose}>{t("agentRunRow.cancel")}</button><button className="isPrimary" type="submit" disabled={!name.trim() || busy}>{busy ? t("shellSidebar.creatingProject") : t("shellSidebar.createProject")}</button></footer>
    </form>
  </div>, document.body);
}

export function ConversationHistory({ sessionProps, recentNavigation, onStartNewChat, onOpenSession, children, label = "会话历史", className = "workspacePanelToggle", side = "top" }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [group, setGroup] = useState("");
  const [creating, setCreating] = useState(false);
  const sessions = sessionProps.sessions.filter(item => item.title.toLocaleLowerCase().includes(query.toLocaleLowerCase()) && (!group || (group === "pinned" ? item.isPinned : item.projectId === group)));
  function select(event) {
    const link = event.target.closest("a[data-session-id]");
    if (!link || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const session = sessions.find(item => item.id === link.dataset.sessionId);
    if (!session) return;
    event.preventDefault(); event.stopPropagation(); setOpen(false); onOpenSession(session);
  }
  return <>
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger className={className} aria-label={label}>{children}</Popover.Trigger>
      <Popover.Portal><Popover.Positioner side={side} align="end" sideOffset={8} className="conversationHistoryPositioner">
        <Popover.Popup className="conversationHistory" aria-label="会话历史">
          <div className="conversationHistorySearch"><Search /><input autoFocus aria-label="筛选会话" placeholder="搜索会话…" value={query} onChange={event => setQuery(event.target.value)} /></div>
          {sessionProps.projects.length ? <select aria-label="会话分组" value={group} onChange={event => setGroup(event.target.value)}><option value="">全部会话</option><option value="pinned">已置顶</option>{sessionProps.projects.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select> : null}
          <nav className="conversationHistoryList" aria-label="历史会话" onClickCapture={select}>{sessions.map(item => sessionProps.renderSessionRow(item, { icon: true }))}</nav>
          {recentNavigation?.loading ? <p role="status">正在加载会话…</p> : recentNavigation?.error ? <p role="alert">无法加载会话，请刷新重试。</p> : !sessions.length ? <p>{query ? "没有匹配的会话" : "暂无会话"}</p> : null}
          <footer><button onClick={() => { setOpen(false); onStartNewChat(); }}><SquarePen />新建会话</button><button onClick={() => { setOpen(false); setCreating(true); }}><Plus />新建分组</button></footer>
        </Popover.Popup>
      </Popover.Positioner></Popover.Portal>
    </Popover.Root>
    {creating ? <ProjectCreateDialog onClose={() => setCreating(false)} onCreate={sessionProps.onCreateProject} /> : null}
  </>;
}
