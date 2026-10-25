export type SessionAttention = Readonly<{
  id: string;
  workspaceId: string;
  agentId: string;
  isUnread: boolean;
  isManuallyUnread: boolean;
  latestAttentionSequence: string;
  readThroughSequence: string;
  attentionRevision: string;
}>;

export function sessionAttentionSequence(value: unknown): bigint {
  if (typeof value !== "string" || value.length > 20 || !/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new Error("session attention sequence invalid");
  }
  const parsed = BigInt(value);
  if (parsed > 18_446_744_073_709_551_615n || parsed.toString() !== value) {
    throw new Error("session attention sequence invalid");
  }
  return parsed;
}

export function validateSessionAttention(value: unknown): asserts value is SessionAttention {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("session attention identity invalid");
  const record = value as Record<string, unknown>;
  if (![record.id, record.workspaceId, record.agentId].every(identity => typeof identity === "string" && identity.length > 0 && identity.trim() === identity)
    || typeof record.isManuallyUnread !== "boolean" || typeof record.isUnread !== "boolean") {
    throw new Error("session attention identity invalid");
  }
  const latest = sessionAttentionSequence(record.latestAttentionSequence);
  const read = sessionAttentionSequence(record.readThroughSequence);
  sessionAttentionSequence(record.attentionRevision);
  if (record.isUnread !== (record.isManuallyUnread || latest > read)) throw new Error("session attention unread state invalid");
}
