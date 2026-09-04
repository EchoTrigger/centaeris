export type UiSession = {
  id: string;
  title: string;
  summary?: string;
  updatedAt?: number;
  isPinned?: boolean;
  isUnread?: boolean;
  messageCount: number;
  cwd?: string;
  sortOrder?: number;
  sessionKind: "main" | "subagent";
  parentSessionId?: string;
  runtimeJobId?: string;
};
