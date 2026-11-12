import { useState } from "react";
import { createPortal } from "react-dom";
import { Popover } from "@centaeris/ui/primitives/popover";
import { SquarePen, Plus, X } from "lucide-react";
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

export function ConversationHistory({ sessionProps, recentNavigation, onStartNewChat, onOpenSession, children, label, className = "workspacePanelToggle", side = "top" }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [group, setGroup] = useState("");
  const [creating, setCreating] = useState(false);
  const sessions = sessionProps.sessions.filter(item => (!group || (group === "pinned" ? item.isPinned : item.projectId === group)));
  function select(event) {
    const link = event.target.closest("a[data-session-id]");
    if (!link || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const session = sessions.find(item => item.id === link.dataset.sessionId);
    if (!session) return;
    event.preventDefault(); event.stopPropagation(); setOpen(false); onOpenSession(session);
  }
  return <>
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger className={className} aria-label={label || t("work.conversation.history")}>{children}</Popover.Trigger>
      <Popover.Portal><Popover.Positioner side={side} align="end" sideOffset={8} className="conversationHistoryPositioner">
        <Popover.Popup className="conversationHistory" aria-label={t("work.conversation.history")}>
          {sessionProps.projects.length ? <select aria-label={t("work.conversation.group")} value={group} onChange={event => setGroup(event.target.value)}><option value="">{t("work.conversation.all")}</option><option value="pinned">{t("work.conversation.pinned")}</option>{sessionProps.projects.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select> : null}
          <nav className="conversationHistoryList" aria-label={t("work.conversation.list")} onClickCapture={select}>{sessions.map(item => sessionProps.renderSessionRow(item, { icon: true }))}</nav>
          {recentNavigation?.loading ? <p role="status">{t("work.conversation.loading")}</p> : recentNavigation?.error ? <p role="alert">{t("work.conversation.error")}</p> : !sessions.length ? <p>{t("work.conversation.empty")}</p> : null}
          <footer><button onClick={() => { setOpen(false); onStartNewChat(); }}><SquarePen />{t("work.conversation.new")}</button><button onClick={() => { setOpen(false); setCreating(true); }}><Plus />{t("work.conversation.newGroup")}</button></footer>
        </Popover.Popup>
      </Popover.Positioner></Popover.Portal>
    </Popover.Root>
    {creating ? <ProjectCreateDialog onClose={() => setCreating(false)} onCreate={sessionProps.onCreateProject} /> : null}
  </>;
}
