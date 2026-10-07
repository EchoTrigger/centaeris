import { AgentMessageBubble } from "./AgentMessageBubble";
import { agentChatTimeline } from "./agentChatTime";
import type { AgentMessageView, PreviewFile } from "./agentChatViewTypes";

export function AgentMessageList({ messages, referenceTime, readLabel, locale, timeZone, onPreviewSession, onPreviewFile }: Readonly<{
  messages: readonly AgentMessageView[];
  // The host captures browser time for relative labels. It never becomes a fact,
  // an ordering key or a substitute for a message's canonical absolute instant.
  referenceTime: string;
  readLabel: string;
  locale?: string;
  timeZone?: string;
  onPreviewSession: (sessionId: string) => void;
  onPreviewFile?: PreviewFile;
}>) {
  return <div className="agentChatMessageList">
    {agentChatTimeline(messages, referenceTime, locale, timeZone).map(({ message, separator }) => <div className="agentChatMessageItem" key={`${message.role}:${message.messageId}`}>
      {separator ? <time className="agentChatTimeSeparator" dateTime={separator.instant} title={separator.full} aria-label={separator.full}>{separator.label}</time> : null}
      <AgentMessageBubble message={message} readLabel={readLabel} locale={locale} timeZone={timeZone} onPreviewSession={onPreviewSession} onPreviewFile={onPreviewFile} />
    </div>)}
  </div>;
}
