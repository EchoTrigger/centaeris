import { Tabs } from "@centaeris/ui/primitives/tabs";
import { useTranslation } from "../i18n";

export function ConversationTabs({ sessions, activeId, expanded, onSelect, onMinimize, panelId }) {
  const { t } = useTranslation();
  if (!sessions.length) return null;
  return <Tabs.Root className="conversationTabs" value={expanded ? activeId : null} onValueChange={id => onSelect(sessions.find(session => session.id === id))}>
    <Tabs.List className="conversationTabList" aria-label={t("work.conversation.toolbar")} activateOnFocus={false}>
      {sessions.map(session => <Tabs.Tab key={session.id} value={session.id} className="conversationDockSession" aria-controls={panelId} title={session.title}
        onClick={() => { if (expanded && session.id === activeId) onMinimize(); }}>
        <span>{session.title || t("work.conversation.untitled")}</span>
      </Tabs.Tab>)}
    </Tabs.List>
  </Tabs.Root>;
}
