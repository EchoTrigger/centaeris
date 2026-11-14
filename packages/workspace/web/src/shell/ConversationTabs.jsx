import { Tabs } from "@centaeris/ui/primitives/tabs";
import { X } from "lucide-react";
import { useTranslation } from "../i18n";

export function ConversationTabs({ sessions, activeId, expanded, onSelect, onMinimize, onClose, panelId }) {
  const { t } = useTranslation();
  if (!sessions.length) return null;
  return <Tabs.Root className="conversationTabs" value={expanded ? activeId : null} onValueChange={id => onSelect(sessions.find(session => session.id === id))}>
    <Tabs.List className="conversationTabList" aria-label={t("work.conversation.toolbar")} activateOnFocus={false}>
      {sessions.map(session => <div className="conversationTabItem" key={session.id} data-selected={expanded && session.id === activeId ? "" : undefined}><Tabs.Tab value={session.id} className="conversationDockSession" aria-controls={panelId} title={session.title}
        onClick={() => { if (expanded && session.id === activeId) onMinimize(); }}>
        <span>{session.title || t("work.conversation.untitled")}</span>
      </Tabs.Tab><button className="conversationTabClose" aria-label={t("work.conversation.closeTab", { title: session.title || t("work.conversation.untitled") })} onClick={() => onClose(session.id)}><X aria-hidden="true" /></button></div>)}
    </Tabs.List>
  </Tabs.Root>;
}
