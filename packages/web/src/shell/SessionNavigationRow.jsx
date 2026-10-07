import { Link } from "react-router";
import { Ellipsis, LoaderCircle, Mail, MailOpen, MessageSquare, Pencil, Pin, PinOff, Trash2 } from "lucide-react";
import { useTranslation } from "../i18n";
import { sessionChatPath } from "../agent-chat/agentChatNavigation";

export function SessionNavigationRow({ workspaceId, session, actions, active = false, running = session.hasActiveAgentRun, icon = false, nested = false, onSelect }) {
  const { t } = useTranslation();
  const isSaving = actions.savingSessionId === session.id;
  const menuOpen = actions.openSessionMenuId === session.id;
  if (actions.editingSessionId === session.id) return <form className="workspaceSessionRow workspaceSessionEdit" onSubmit={event => { event.preventDefault(); event.currentTarget.elements.title.blur(); }}>
    <input name="title" autoComplete="off" aria-label={t("appRoute.renameValue", { value1: session.title })} autoFocus defaultValue={session.title} maxLength={200} disabled={isSaving}
      onBlur={event => actions.renameSession(session, event.currentTarget.value)}
      onKeyDown={event => { if (event.key === "Enter" && event.nativeEvent.isComposing) event.preventDefault(); if (event.key === "Escape") { event.preventDefault(); event.currentTarget.value = session.title; event.currentTarget.blur(); } }} />
  </form>;
  return <div className={`workspaceSessionRow ${nested ? "isNested" : ""} ${menuOpen ? "hasOpenMenu" : ""}`}>
    <Link className={`workspaceSessionButton ${active ? "isActive" : ""}`} to={sessionChatPath(workspaceId, session.agentId, session.id)} state={{ sidebarTab: "chat" }}
      aria-current={active ? "page" : undefined} data-testid={active ? "active-session" : undefined} data-session-id={session.id}
      onClick={event => { if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) onSelect?.(session); }}>
      {icon ? <MessageSquare className="workspaceSessionKindIcon" aria-hidden="true" /> : null}
      <span className="workspaceSessionContent">
        <span className="workspaceSessionTitle"><span>{session.title}</span>{session.isPinned ? <Pin aria-label={t("appRoute.pinned")} /> : null}</span>
        <span className="workspaceSessionMetadata">
          {session.isUnread ? <span className="workspaceSessionUnread" aria-label={t("appRoute.unread")} /> : null}
        </span>
      </span>
    </Link>
    {running ? <span className="workspaceAgentRunning" aria-label={t("appRoute.running")}><LoaderCircle aria-hidden="true" /></span> : null}
    <div className="workspaceSessionActions">
      <button className="workspaceSessionActionButton" type="button" aria-label={t("appRoute.conversationActionsForValue", { value1: session.title })} aria-expanded={menuOpen}
        onClick={() => actions.setOpenSessionMenuId(current => current === session.id ? "" : session.id)}><Ellipsis aria-hidden="true" /></button>
      {menuOpen ? <div className="workspaceSessionMenu" role="menu">
        <button type="button" role="menuitem" onClick={() => actions.onRename(session)}><Pencil aria-hidden="true" />{t("appRoute.rename")}</button>
        <button type="button" role="menuitem" disabled={isSaving} onClick={() => actions.onTogglePin(session)}>{session.isPinned ? <PinOff aria-hidden="true" /> : <Pin aria-hidden="true" />}{session.isPinned ? t("appRoute.unpin") : t("appRoute.pin")}</button>
        <button type="button" role="menuitem" disabled={isSaving} onClick={() => actions.onToggleUnread(session)}>{session.isUnread ? <MailOpen aria-hidden="true" /> : <Mail aria-hidden="true" />}{session.isUnread ? t("appRoute.markAsRead") : t("appRoute.markAsUnread")}</button>
        <button className="isDanger" type="button" role="menuitem" disabled={Boolean(actions.deletingSessionId)} onClick={() => actions.onRequestDelete(session)}><Trash2 aria-hidden="true" />{t("appRoute.delete")}</button>
      </div> : null}
    </div>
  </div>;
}
