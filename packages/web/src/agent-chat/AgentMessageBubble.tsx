import { MarkdownContent } from "../chat/MarkdownContent";
import { AgentSessionReference } from "./AgentSessionReference";
import { AgentMessageFiles } from "./AgentPreviewContent";
import { AgentInputAttachments } from "./AgentInputAttachments";
import { formatSuppliedTime } from "./agentChatTime";
import type { AgentMessageView, PreviewFile } from "./agentChatViewTypes";

export function AgentMessageBubble({ message, readLabel, locale, timeZone, onPreviewSession, onPreviewFile }: Readonly<{
  message: AgentMessageView;
  readLabel: string;
  locale?: string;
  timeZone?: string;
  onPreviewSession: (sessionId: string) => void;
  onPreviewFile?: PreviewFile;
}>) {
  const inputFact = message.role === "user" ? message.loopInputFact : undefined;
  const hasMatchingInput = message.role === "user" && inputFact?.kind === "loopInput" && inputFact.messageId === message.messageId;
  const timestamp = message.isLatest && hasMatchingInput
    ? formatSuppliedTime(inputFact?.receivedAt, locale, timeZone)
    : null;
  const sessions = message.role === "agent" ? message.sessions : undefined;
  return <div className={`agentChatMessageGroup agentChatMessageGroup--${message.role}`}>
    <article className={`agentChatMessage agentChatMessage--${message.role}`} aria-label={message.authorLabel}>
    <div className="agentChatMessageBody">
      {message.role === "agent" ? <MarkdownContent text={message.text} /> : <div className="agentChatUserText">{message.text}</div>}
    </div>
    {message.role === "user" && message.attachments?.length && message.attachmentBinding ? <AgentInputAttachments attachments={message.attachments} {...message.attachmentBinding} /> : null}
    {message.role === "agent" && message.fileBinding && onPreviewFile ? <AgentMessageFiles messageId={message.messageId} binding={message.fileBinding} onPreviewFile={onPreviewFile} /> : null}
    </article>
    {timestamp ? <footer className="agentChatMessageFooter">
      <span className="agentChatMessageTime">
        <span>{readLabel} </span>
        <time dateTime={timestamp.instant} title={timestamp.full} aria-label={timestamp.full}>{timestamp.label}</time>
      </span>
    </footer> : null}
    {sessions?.length ? <div className="agentChatSessionReferences">
      {sessions.map(session => <AgentSessionReference key={session.sessionId} session={session} onPreview={onPreviewSession} />)}
    </div> : null}
  </div>;
}
