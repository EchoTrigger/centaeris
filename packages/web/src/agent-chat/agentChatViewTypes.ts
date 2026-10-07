import type { ReactNode } from "react";
import type { FilePreviewFact, MessageFileBinding } from "./agentPreviewData";
import type { AgentInputAttachment } from "./preparedAgentInput";

export type FilePreviewRequest = Readonly<{ file: FilePreviewFact; invalidateMetadata: () => void }>;
export type PreviewFile = (request: FilePreviewRequest, sessionId: string) => void;

// Local, readonly presentation inputs. These are not public transport contracts.
// The host supplies authoritative identities, latest-by-role selection and facts.
export type AgentSessionReferenceView = Readonly<{
  sessionId: string;
  title: string;
  runState: "running" | "completed" | "unknown" | "failed" | "cancelled";
}>;

export type AgentLoopInputFactView = Readonly<{
  kind: "loopInput";
  messageId: string;
  receivedAt: string;
}>;

type MessageView = Readonly<{
  messageId: string;
  authorLabel: string;
  text: string;
  isLatest: boolean;
  // Server canonical message instant, adapted by the host. Display grouping only;
  // never derive this from Read facts, arrival time, array position or client now.
  createdAt?: string;
}>;

export type AgentMessageView =
  | (MessageView & Readonly<{ role: "user"; loopInputFact?: AgentLoopInputFactView; attachments?: readonly AgentInputAttachment[]; attachmentBinding?: Readonly<{ agentId: string; inputId: string }> }> )
  | (MessageView & Readonly<{ role: "agent"; sessions?: readonly AgentSessionReferenceView[]; fileBinding?: MessageFileBinding }>);

export type AgentPreviewLoadView =
  | Readonly<{ status: "loading" }>
  | Readonly<{ status: "error"; error: string }>
  | Readonly<{ status: "ready" }>;

export type AgentPreviewLabels = Readonly<{
  preview: string;
  path: string;
  close: string;
  loading: string;
  retry: string;
  returnToSession: string;
}>;

export type AgentLibraryPreviewView = Readonly<{
  title: string;
  // The host adapts its existing Library preview here; this shell owns no renderer.
  content: ReactNode;
}>;

export type AgentSessionPreviewShellProps = Readonly<{
  // The integrated route owns one stable frame across overview/Session views.
  embedded?: boolean;
  session: AgentSessionReferenceView;
  agentLabel: string;
  labels: AgentPreviewLabels;
  load: AgentPreviewLoadView;
  conversation: ReactNode;
  libraryPreview?: AgentLibraryPreviewView;
  onReturnToSession: () => void;
  onReturn: () => void;
  onClose: () => void;
  onRetry?: () => void;
}>;
