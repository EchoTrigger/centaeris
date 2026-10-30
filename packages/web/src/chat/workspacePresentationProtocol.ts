import { validateSessionAttention, type SessionAttention } from '../sessionAttentionContract.ts';
import { hasExactFields, isInteger, isRecord, requireString, validateLiveReasoning } from './sessionStreamProtocol.ts';
import { validateTranscriptPage, type TranscriptPage, type TranscriptPatchPage, type TranscriptContentRange } from './transcriptContract.ts';

export type PresentationIdentity = Readonly<{ workspaceId: string; agentId: string; sessionId: string | null }>;
export type PresentationSession = SessionAttention & Readonly<Record<string, unknown>>;
export type PresentationAgentRun = Readonly<{ agentRunId: string; status: 'queued' | 'running' | 'completed' | 'failed' | 'cancelled'; startedAt: string | null; completedAt: string | null }>;
export type PresentationOperation = Readonly<{
  kind: 'call' | 'result'; callId: string; toolName: string; normalizedInput: Record<string, unknown> | null;
  resultState: string | null; operations: readonly Record<string, unknown>[]; previewTruncated: boolean; sourceEventId: string;
}>;
type Common = PresentationIdentity & Readonly<{ schema: 'workspace.presentation.v1' }>;
export type PresentationViewRevision = Common & Readonly<{ kind: 'viewRevision'; contextRevision: string; historyRevision: string; workRevision: string }>;
export type PresentationCheckpoint = Common & Readonly<{ kind: 'checkpoint'; projectionGeneration: string | null; sourceHighWater: string; agentRun: PresentationAgentRun | null }>;
export type PresentationFrame =
  | PresentationViewRevision
  | (Common & Readonly<{ kind: 'navigation'; snapshotId: string; offset: number; hasMore: boolean; sessions: readonly PresentationSession[] }>)
  | (Common & Readonly<{ kind: 'snapshot'; page: TranscriptPage }>)
  | (Common & Readonly<{ kind: 'patches'; page: TranscriptPatchPage }>)
  | (Common & Readonly<{ kind: 'content'; range: TranscriptContentRange }>)
  | (Common & Readonly<{ kind: 'live'; agentRunId: string; live: Readonly<{ afterSequence: number; revision: number; turnId: string; messageId: string; text: string; reasoning?: Readonly<{ blockId: string; requestId: string; text: string }> | null }> | null }>)
  | (Common & Readonly<{ kind: 'operation'; detail: PresentationOperation }>)
  | PresentationCheckpoint
  | (Common & Readonly<{ kind: 'reset'; reason: 'cursorInvalid' | 'projectionInvalidated' }>);
export type PresentationEntry = Readonly<{ cursor: string | null; item: PresentationFrame }>;
const COMMON = ['schema', 'kind', 'workspaceId', 'agentId', 'sessionId'];
const FIELDS: Record<PresentationFrame['kind'], readonly string[]> = {
  viewRevision: ['contextRevision', 'historyRevision', 'workRevision'],
  navigation: ['snapshotId', 'offset', 'hasMore', 'sessions'], snapshot: ['page'], patches: ['page'], content: ['range'],
  live: ['agentRunId', 'live'], operation: ['detail'], checkpoint: ['projectionGeneration', 'sourceHighWater', 'agentRun'], reset: ['reason'],
};
const SESSION_FIELDS = ['id', 'workspaceId', 'agentId', 'projectId', 'currentEnvironmentId', 'executionMode', 'title', 'origin', 'initialInputOrigin', 'status', 'deletedAt', 'isPinned', 'isUnread', 'hasActiveAgentRun', 'updatedAt', 'latestAttentionSequence', 'readThroughSequence', 'attentionRevision', 'isManuallyUnread'];
const DECIMAL = /^(0|[1-9][0-9]*)$/;
export const PRESENTATION_MAX_FRAME_BYTES = 4 * 1024 * 1024;
export function isPresentationWater(value: unknown): value is string {
  return typeof value === 'string' && DECIMAL.test(value) && BigInt(value) <= 18_446_744_073_709_551_615n;
}
export class PresentationProtocolError extends Error {}
function invalid(message = 'invalid presentation frame'): never { throw new PresentationProtocolError(message); }

export function validatePresentationFrame(value: unknown, identity: PresentationIdentity): PresentationFrame {
  if (!isRecord(value) || value.schema !== 'workspace.presentation.v1' || typeof value.kind !== 'string'
    || !Object.hasOwn(FIELDS, value.kind) || !hasExactFields(value, [...COMMON, ...FIELDS[value.kind as PresentationFrame['kind']]])
    || value.workspaceId !== identity.workspaceId || value.agentId !== identity.agentId || value.sessionId !== identity.sessionId) invalid();
  switch (value.kind) {
    case 'viewRevision':
      if (![value.contextRevision, value.historyRevision, value.workRevision].every(isPresentationWater)) invalid('invalid presentation view revision');
      break;
    case 'navigation': {
      if (!Array.isArray(value.sessions) || typeof value.snapshotId !== 'string' || !value.snapshotId
        || !Number.isSafeInteger(value.offset) || Number(value.offset) < 0 || typeof value.hasMore !== 'boolean') invalid('invalid presentation navigation');
      const ids = new Set<string>();
      for (const row of value.sessions) {
        if (!isRecord(row) || !hasExactFields(row, SESSION_FIELDS) || typeof row.id !== 'string' || !row.id
          || row.workspaceId !== identity.workspaceId || typeof row.agentId !== 'string' || !row.agentId || row.agentId.trim() !== row.agentId
          || typeof row.isPinned !== 'boolean'
          || typeof row.hasActiveAgentRun !== 'boolean' || typeof row.title !== 'string' || ids.has(row.id)) invalid('invalid presentation navigation');
        try { validateSessionAttention(row); } catch { invalid("invalid presentation navigation attention"); }
        ids.add(row.id);
      }
      break;
    }
    case 'snapshot':
      if (identity.sessionId === null) invalid();
      validateTranscriptPage(value.page, { sessionId: identity.sessionId });
      break;
    case 'patches':
      // Full patch identity and continuity are validated against the applied store.
      if (!isRecord(value.page) || identity.sessionId === null || value.page.sessionId !== identity.sessionId) invalid();
      break;
    case 'content':
      // Reference revision, exact keys and offsets are validated by the owning content slot.
      if (!isRecord(value.range) || identity.sessionId === null || value.range.sessionId !== identity.sessionId) invalid();
      break;
    case 'live': {
      requireString(value.agentRunId, 'presentation agentRunId');
      if (value.live === null) break;
      const live = value.live;
      if (!isRecord(live) || !hasExactFields(live, ['afterSequence', 'revision', 'turnId', 'messageId', 'text', ...(Object.hasOwn(live, 'reasoning') ? ['reasoning'] : [])])
        || !isInteger(live.afterSequence) || live.afterSequence < 0 || !isInteger(live.revision) || live.revision <= 0
        || typeof live.text !== 'string') invalid('invalid presentation live text');
      for (const key of ['turnId', 'messageId']) requireString(live[key], `presentation live ${key}`);
      validateLiveReasoning(live.reasoning);
      if (new TextEncoder().encode(JSON.stringify(live)).byteLength > PRESENTATION_MAX_FRAME_BYTES) invalid('presentation live frame is too large');
      break;
    }
    case 'operation': {
      const detail = value.detail;
      if (!isRecord(detail) || !hasExactFields(detail, ['kind', 'callId', 'toolName', 'normalizedInput', 'resultState', 'operations', 'previewTruncated', 'sourceEventId'])
        || !['call', 'result'].includes(String(detail.kind)) || typeof detail.callId !== 'string' || !detail.callId
        || typeof detail.toolName !== 'string' || !detail.toolName || typeof detail.sourceEventId !== 'string' || !detail.sourceEventId
        || (detail.normalizedInput !== null && !isRecord(detail.normalizedInput))
        || (detail.resultState !== null && typeof detail.resultState !== 'string')
        || !Array.isArray(detail.operations) || !detail.operations.every(isRecord) || typeof detail.previewTruncated !== 'boolean'
        || new TextEncoder().encode(JSON.stringify(detail)).byteLength > 512 * 1024) invalid('invalid presentation operation');
      break;
    }
    case 'checkpoint': {
      if (!isPresentationWater(value.sourceHighWater) || (value.projectionGeneration !== null
        && (typeof value.projectionGeneration !== 'string' || !value.projectionGeneration))) invalid('invalid presentation checkpoint');
      if (value.agentRun !== null) {
        const run = value.agentRun;
        if (!isRecord(run) || !hasExactFields(run, ['agentRunId', 'status', 'startedAt', 'completedAt'])
          || typeof run.agentRunId !== 'string' || !run.agentRunId
          || !['queued', 'running', 'completed', 'failed', 'cancelled'].includes(String(run.status))
          || [run.startedAt, run.completedAt].some(time => time !== null && (typeof time !== 'string' || !Number.isFinite(Date.parse(time))))) invalid('invalid presentation AgentRun');
      }
      break;
    }
    case 'reset':
      if (value.reason !== 'cursorInvalid' && value.reason !== 'projectionInvalidated') invalid('invalid presentation reset');
      break;
  }
  return value as PresentationFrame;
}

export function readPresentationSseBlock(block: string, identity: PresentationIdentity): PresentationEntry | null {
  let cursor: string | null = null;
  const data: string[] = [];
  for (const line of block.split(/\r?\n/)) {
    if (line.startsWith(':') || line === '') continue;
    if (line.startsWith('id:')) {
      cursor = line.slice(3).trimStart();
      if (!cursor || cursor.length > 4096 || /[\u0000\r\n]/.test(cursor)) invalid('invalid presentation cursor');
    } else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
    else if (line.startsWith('event:') && line.slice(6).trim() !== 'message') invalid('unsupported presentation event');
  }
  if (data.length === 0) {
    if (cursor !== null) invalid('presentation cursor requires data');
    return null;
  }
  let value: unknown;
  try { value = JSON.parse(data.join('\n')); } catch { invalid('invalid presentation JSON'); }
  const item = validatePresentationFrame(value, identity);
  if (cursor !== null && item.kind !== 'checkpoint') invalid('only an applied checkpoint can carry a cursor');
  return { cursor, item };
}
