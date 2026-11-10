export type TerminalSessionEventType =
  | "agent_run_completed"
  | "agent_run_failed"
  | "agent_run_interrupted";

export type UnknownRecord = Record<string, unknown>;

export type SessionStreamEvent = UnknownRecord & {
  schemaVersion: "session.event.v1";
  eventVersion: 1;
  sequence: number;
  type: string;
  eventId: string;
  sessionId: string;
  agentRunId?: string;
  turnId?: string;
  createdAtMs: number;
  payload: UnknownRecord;
};

export type CommittedStreamItem = UnknownRecord & {
  schema: "session.stream.item.v1";
  kind: "committed";
  agentRunId: string;
  sourceSequence: number;
  event: SessionStreamEvent;
};

export type LiveStreamItem = UnknownRecord & {
  reasoning?: { blockId: string; requestId: string; text: string } | null;
  schema: "session.stream.item.v1";
  kind: "live";
  agentRunId: string;
  messageId: string;
  turnId: string;
  afterSequence: number;
  revision: number;
  text: string;
};

export type StreamItem = CommittedStreamItem | LiveStreamItem;

export type StreamEntry = {
  cursor: string | null;
  item: StreamItem;
};
