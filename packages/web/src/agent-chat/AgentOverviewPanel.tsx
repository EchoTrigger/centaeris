import { useState } from "react";
import { useTranslation } from "../i18n";
import type { AgentSummary } from "../router";
import { AgentMark } from "../shell/AgentMark";
import { AgentOverviewSessions } from "./AgentPreviewContent";
import type { ReferencedSession } from "./agentOutputFeed";
import type { PreviewFile } from "./agentChatViewTypes";

export function AgentOverviewPanel({ agent, revision, sessions, pagination, onPreviewSession, onPreviewFile, onRunState }: Readonly<{
  agent: AgentSummary; sessions: readonly ReferencedSession[];
  revision?: string;
  pagination?: Readonly<{ hasMore: boolean; loading: boolean; error: boolean; onLoadMore: () => void; onRetry: () => void }>;
  onPreviewSession: (sessionId: string) => void; onPreviewFile: PreviewFile;
  onRunState: (sessionId: string, state: "running" | "unknown") => void;
}>) {
  const { t } = useTranslation();
  const [showAll, setShowAll] = useState(false);
  const visible = showAll ? sessions : sessions.slice(0, 3);
  return <>
    <header className="agentChatOverviewHeader">
      <AgentMark className="agentChatOverviewAvatar" agent={agent} />
      <div><strong>{agent.name}</strong></div>
    </header>
    <div className="agentChatPreviewBody">
      <AgentOverviewSessions sessions={visible} revision={revision} onPreviewSession={onPreviewSession} onPreviewFile={onPreviewFile} onRunState={onRunState} />
      {pagination?.error ? <p role="alert">{t("agentChat.previewError")} <button type="button" onClick={pagination.onRetry}>{t("agentChat.retry")}</button></p> : null}
      {sessions.length > visible.length || pagination?.hasMore ? <button className="agentChatShowMore" type="button" disabled={pagination?.loading} onClick={() => !showAll && sessions.length > visible.length ? setShowAll(true) : pagination?.onLoadMore()}>{t("agentChat.showMore")}</button> : null}
    </div>
  </>;
}
