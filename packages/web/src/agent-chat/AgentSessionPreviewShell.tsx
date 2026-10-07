import { ChevronLeft, X } from "lucide-react";
import { Activity, useEffect, useRef } from "react";
import { AgentPreviewPanel } from "./AgentPreviewPanel";
import type { AgentSessionPreviewShellProps } from "./agentChatViewTypes";

export function AgentSessionPreviewShell({
  session, agentLabel, labels, load, conversation, libraryPreview, embedded = false,
  onReturnToSession, onReturn, onClose, onRetry,
}: AgentSessionPreviewShellProps) {
  const fileBodyRef = useRef<HTMLDivElement>(null);
  const sessionBodyRef = useRef<HTMLElement>(null);
  const fileTitle = libraryPreview?.title;
  useEffect(() => {
    if (!fileTitle) return;
    const trigger = document.activeElement;
    const frame = requestAnimationFrame(() => fileBodyRef.current?.focus({ preventScroll: true }));
    return () => {
      cancelAnimationFrame(frame);
      requestAnimationFrame(() => {
        if (!sessionBodyRef.current?.isConnected || sessionBodyRef.current.hidden) return;
        if (trigger instanceof HTMLElement && trigger.isConnected && trigger.getClientRects().length) trigger.focus({ preventScroll: true });
        else if (document.activeElement === document.body) sessionBodyRef.current.focus({ preventScroll: true });
      });
    };
  }, [fileTitle]);
  const content = <>
    <header className="agentChatPreviewHeader">
      <button className="agentChatPreviewBack" type="button" onClick={libraryPreview ? onReturnToSession : onReturn}
        aria-label={libraryPreview ? labels.returnToSession : agentLabel} title={libraryPreview ? session.title : agentLabel}><ChevronLeft aria-hidden="true" /></button>
      <strong className="agentChatPreviewTitle" title={fileTitle ?? session.title}>{fileTitle ?? session.title}</strong>
      <button className="agentChatPreviewClose" type="button" onClick={onClose} aria-label={labels.close} title={labels.close}><X aria-hidden="true" /></button>
    </header>
    <Activity mode={libraryPreview ? "hidden" : "visible"}><section className="agentChatPreviewBody agentChatConversationPreview" ref={sessionBodyRef} tabIndex={0} aria-label={session.title} hidden={Boolean(libraryPreview)}>
      {load.status === "error" ? <div className="agentChatPreviewState agentChatPreviewState--error" role="alert"><p>{load.error}</p>{onRetry ? <button type="button" onClick={onRetry}>{labels.retry}</button> : null}</div> : null}
      {load.status === "ready" ? conversation : null}
    </section></Activity>
    {libraryPreview ? <div className="agentChatPreviewBody agentChatLibraryPreview" ref={fileBodyRef} tabIndex={-1} aria-label={libraryPreview.title}>{libraryPreview.content}</div> : null}
  </>;
  return embedded ? content : <AgentPreviewPanel label={labels.preview} focusKey={session.sessionId} onClose={onClose}>{content}</AgentPreviewPanel>;
}
