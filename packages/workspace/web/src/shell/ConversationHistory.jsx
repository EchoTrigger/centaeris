import { useState } from "react";
import { Link } from "react-router";
import { Popover } from "@centaeris/ui/primitives/popover";
import { Plus } from "lucide-react";
import { useTranslation } from "../i18n";
import { sessionChatPath } from "./sessionNavigation.mjs";
import "./conversation-dock.css";

function elapsedTime(updatedAt, now, t) {
  const seconds = Math.max(0, Math.floor((now.getTime() - new Date(updatedAt).getTime()) / 1000));
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return t("chat.time.now");
  for (const [unit, divisor] of [["week", 604800], ["day", 86400], ["hour", 3600], ["minute", 60]]) {
    if (seconds >= divisor) return t(`chat.time.${unit}`, { count: Math.floor(seconds / divisor) });
  }
}

export function ConversationHistory({ sessionProps, workspaceId, currentSessionId, recentNavigation, onStartNewChat, onOpenSession, children, label, className = "workspacePanelToggle", side = "bottom" }) {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
  const now = new Date();
  const sessions = [...sessionProps.sessions].sort((a, b) => String(b.updatedAt || "").localeCompare(String(a.updatedAt || "")));
  const weekStart = new Date(now); weekStart.setHours(0, 0, 0, 0);
  weekStart.setDate(weekStart.getDate() - (weekStart.getDay() + 6) % 7);
  const lastWeekStart = new Date(weekStart); lastWeekStart.setDate(lastWeekStart.getDate() - 7);
  const groups = [[], [], []];
  for (const session of sessions) {
    const updatedAt = new Date(session.updatedAt).getTime();
    groups[!Number.isFinite(updatedAt) || updatedAt >= weekStart.getTime() ? 0 : updatedAt >= lastWeekStart.getTime() ? 1 : 2].push(session);
  }
  return <Popover.Root open={open} onOpenChange={setOpen}>
    <Popover.Trigger className={className} aria-label={label || t("work.conversation.history")}>{children}</Popover.Trigger>
    <Popover.Portal><Popover.Positioner side={side} align="start" sideOffset={5} className="conversationHistoryPositioner">
      <Popover.Popup className="conversationHistory" aria-label={t("work.conversation.history")}>
        <button className="conversationHistoryNew" onClick={() => { setOpen(false); onStartNewChat(); }}><Plus aria-hidden="true" />{t("work.conversation.new")}</button>
        <nav className="conversationHistoryList" aria-label={t("work.conversation.list")}>
          {groups.map((group, index) => group.length ? <section key={index} className="conversationHistoryGroup" aria-label={index === 1 ? t("chat.group.lastWeek") : undefined}>
            {index === 1 ? <h2>{t("chat.group.lastWeek")}</h2> : null}
            {group.map(session => <Link key={session.id} className="conversationHistoryRow" data-session-id={session.id}
              to={sessionChatPath(workspaceId, session.id)} aria-current={session.id === currentSessionId ? "page" : undefined}
              onClick={event => {
                if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
                event.preventDefault(); setOpen(false); onOpenSession(session);
              }}>
              <span className="conversationHistoryTitle">{session.title || t("work.conversation.untitled")}</span>
              {session.id === currentSessionId ? <span className="conversationHistoryCurrent">{t("chat.current")}</span> : null}
              {session.updatedAt ? <time dateTime={session.updatedAt} title={new Date(session.updatedAt).toLocaleString(i18n.language)}>{elapsedTime(session.updatedAt, now, t)}</time> : null}
            </Link>)}
          </section> : null)}
        </nav>
        {recentNavigation?.loading ? <p role="status">{t("work.conversation.loading")}</p> : recentNavigation?.error ? <p role="alert">{t("work.conversation.error")}</p> : !sessions.length ? <p>{t("work.conversation.empty")}</p> : null}
      </Popover.Popup>
    </Popover.Positioner></Popover.Portal>
  </Popover.Root>;
}
