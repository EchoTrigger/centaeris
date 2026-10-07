const PAGE_SCHEMA = "transcript.page.v1";
const PATCH_PAGE_SCHEMA = "transcript.patch.page.v1";
const PATCH_SCHEMA = "transcript.patch.v1";
const PROJECTION_VERSION = "transcript.projection.v1";
const WORKSPACE_STREAM_ID = "workspace-transcript.v1";
const MAX_BLOCKS = 128;
const MAX_INLINE_BYTES = 64 * 1024;
const MAX_PAGE_BYTES = 4 * MAX_INLINE_BYTES;
const MAX_PATCHES = MAX_BLOCKS;
const MAX_PATCH_CHANGES = MAX_BLOCKS;
const MAX_PATCH_BYTES = MAX_PAGE_BYTES;
const MAX_PATCH_PAGE_BYTES = 2 * MAX_PATCH_BYTES;
const U64_MAX = 18_446_744_073_709_551_615n;

export type TranscriptContentRef = Readonly<{
  refId: string;
  revision: string;
  byteLength: string;
}>;

export type TranscriptContentRange = Readonly<{
  schema: "transcript.content.range.v1";
  sessionId: string;
  projectionVersion: typeof PROJECTION_VERSION;
  projectionGeneration: string;
  refId: string;
  revision: string;
  byteLength: string;
  startOffset: string;
  endOffset: string;
  content: string;
  hasMore: boolean;
}>;

type TranscriptTextContent = Readonly<{
  inlineContent: string | null;
  sourceRef: TranscriptContentRef | null;
}>;

type TranscriptBlockBody = Readonly<Record<string, unknown> & { kind: string }>;

export type TranscriptBlock = Readonly<{
  blockId: string;
  blockRevision: string;
  presentation?: Readonly<{
    agentRunId: string | null;
    sourceType: string;
    observedAtMs: number;
    displayTarget: string | null;
    durationMs: number | null;
    operation: unknown;
  }> | null;
  orderKey: Readonly<{ sourceSequence: string; ordinal: number }>;
  body: TranscriptBlockBody;
}>;

export type TranscriptPage = Readonly<{
  schema: typeof PAGE_SCHEMA;
  sessionId: string;
  projectionVersion: typeof PROJECTION_VERSION;
  projectionGeneration: string;
  sourceHighWater: string;
  blocks: readonly TranscriptBlock[];
  olderCursor: string | null;
  hasOlder: boolean;
  resumeCursors: readonly Readonly<{ streamId: string; cursor: string }>[];
}>;

type TranscriptPatch = Readonly<{
  schema: typeof PATCH_SCHEMA;
  sessionId: string;
  projectionVersion: typeof PROJECTION_VERSION;
  projectionGeneration: string;
  sourceHighWater: string;
  streamId: typeof WORKSPACE_STREAM_ID;
  appliedCursor: string;
  upserts: readonly TranscriptBlock[];
  removals: readonly Readonly<{ blockId: string; blockRevision: string }>[];
}>;

export type TranscriptPatchPage = Readonly<{
  schema: typeof PATCH_PAGE_SCHEMA;
  sessionId: string;
  projectionVersion: typeof PROJECTION_VERSION;
  projectionGeneration: string;
  throughSourceHighWater: string;
  patches: readonly TranscriptPatch[];
  nextSourceHighWater: string;
  hasMore: boolean;
}>;

export type TranscriptListSnapshot = Readonly<{
  sessionId: string | null;
  projectionVersion: string | null;
  projectionGeneration: string | null;
  sourceHighWater: string;
  viewEpoch: number;
  blockIds: readonly string[];
  olderCursor: string | null;
  hasOlder: boolean;
  appliedSourceHighWater: string;
}>;

export type TranscriptLiveOverlay = Readonly<{
  messageId: string;
  turnId: string;
  afterSourceHighWater: string;
  revision: number;
  text: string;
  reasoning: Readonly<{ blockId: string; requestId: string; text: string }> | null;
}>;

export type TranscriptIdentity = Readonly<{
  sessionId: string;
  projectionVersion: string;
  projectionGeneration: string;
  sourceHighWater: string;
}>;

export function validateTranscriptContentRange(
  value: unknown,
  expected: Readonly<{
    sessionId: string;
    projectionGeneration: string;
    reference: TranscriptContentRef;
    offset: string;
  }>,
): TranscriptContentRange {
  if (!isRecord(value)
    || !exactKeys(value, [
      "schema", "sessionId", "projectionVersion", "projectionGeneration", "refId",
      "revision", "byteLength", "startOffset", "endOffset", "content", "hasMore",
    ])
    || value.schema !== "transcript.content.range.v1"
    || value.sessionId !== expected.sessionId
    || value.projectionVersion !== PROJECTION_VERSION
    || value.projectionGeneration !== expected.projectionGeneration
    || value.refId !== expected.reference.refId
    || value.revision !== expected.reference.revision
    || value.byteLength !== expected.reference.byteLength
    || value.startOffset !== expected.offset
    || !decimal(value.endOffset)
    || BigInt(value.endOffset) < BigInt(expected.offset)
    || BigInt(value.endOffset) > BigInt(expected.reference.byteLength)
    || typeof value.content !== "string"
    || utf8Bytes(value.content) > MAX_INLINE_BYTES
    || BigInt(value.endOffset) - BigInt(expected.offset) !== BigInt(utf8Bytes(value.content))
    || typeof value.hasMore !== "boolean"
    || value.hasMore !== (BigInt(value.endOffset) < BigInt(value.byteLength as string))) {
    throw new Error("invalid transcript content range");
  }
  return cloneTranscriptValue(value as TranscriptContentRange);
}

function invalid(kind: "page" | "patch page"): never {
  throw new Error(`invalid transcript ${kind}`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]) {
  const keys = Object.keys(value).sort();
  return keys.length === expected.length
    && keys.every((key, index) => key === [...expected].sort()[index]);
}

function identifier(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function decimal(value: unknown): value is string {
  return typeof value === "string"
    && /^(0|[1-9][0-9]*)$/.test(value)
    && value.length <= 20
    && BigInt(value) <= U64_MAX;
}

function serializedBytes(value: unknown) {
  return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}

function utf8Bytes(value: string) {
  return new TextEncoder().encode(value).byteLength;
}

function validContentRef(value: unknown): value is TranscriptContentRef {
  return isRecord(value)
    && exactKeys(value, ["refId", "revision", "byteLength"])
    && identifier(value.refId)
    && decimal(value.revision)
    && decimal(value.byteLength);
}

function validTextContent(value: unknown): value is TranscriptTextContent {
  if (!isRecord(value) || !exactKeys(value, ["inlineContent", "sourceRef"])) return false;
  return (typeof value.inlineContent === "string"
      && utf8Bytes(value.inlineContent) <= MAX_INLINE_BYTES
      && value.sourceRef === null)
    || (value.inlineContent === null && validContentRef(value.sourceRef));
}

function validStatus(value: unknown) {
  return ["queued", "running", "completed", "failed", "interrupted"].includes(String(value));
}

function validPresentation(value: unknown) {
  if (value === null) return true;
  if (!isRecord(value) || !exactKeys(value, [
    "agentRunId", "sourceType", "observedAtMs", "displayTarget", "durationMs", "operation",
  ])) return false;
  return (value.agentRunId === null || typeof value.agentRunId === "string")
    && typeof value.sourceType === "string" && Number.isSafeInteger(value.observedAtMs)
    && (value.displayTarget === null || typeof value.displayTarget === "string")
    && (value.durationMs === null || (Number.isSafeInteger(value.durationMs) && Number(value.durationMs) >= 0))
    && value.operation !== undefined;
}

function validBlock(value: unknown): value is TranscriptBlock {
  if (!isRecord(value)) return false;
  const fields = ["blockId", "blockRevision", "orderKey", "body"];
  if (Object.hasOwn(value, "presentation")) {
    fields.push("presentation");
    if (!validPresentation(value.presentation)) return false;
  }
  if (!exactKeys(value, fields)) return false;
  if (!identifier(value.blockId) || !decimal(value.blockRevision) || !isRecord(value.orderKey)
    || !exactKeys(value.orderKey, ["sourceSequence", "ordinal"])
    || !decimal(value.orderKey.sourceSequence)
    || !Number.isInteger(value.orderKey.ordinal)
    || Number(value.orderKey.ordinal) < 0
    || Number(value.orderKey.ordinal) > 4_294_967_295
    || !isRecord(value.body)) return false;
  const body = value.body;
  if (body.kind === "userText") {
    return exactKeys(body, ["kind", "content"]) && validTextContent(body.content);
  }
  if (body.kind === "assistantText") {
    return exactKeys(body, ["kind", "content", "status"])
      && validTextContent(body.content) && validStatus(body.status);
  }
  if (body.kind === "reasoning") {
    return exactKeys(body, ["kind", "requestId", "content", "status"])
      && identifier(body.requestId) && validTextContent(body.content) && validStatus(body.status);
  }
  if (body.kind === "notice") {
    return exactKeys(body, ["kind", "noticeType", "content", "status"])
      && identifier(body.noticeType) && validTextContent(body.content) && validStatus(body.status);
  }
  if (body.kind !== "tool" || !exactKeys(body, [
    "kind", "callId", "toolName", "status", "summary", "summaryRef", "outputRef",
  ]) || !identifier(body.callId) || !identifier(body.toolName) || !validStatus(body.status)) return false;
  const summaryValid = (typeof body.summary === "string"
      && utf8Bytes(body.summary) <= MAX_INLINE_BYTES
      && body.summaryRef === null)
    || (body.summary === null && validContentRef(body.summaryRef));
  return summaryValid && (body.outputRef === null || validContentRef(body.outputRef));
}

export function compareTranscriptBlocks(left: TranscriptBlock, right: TranscriptBlock) {
  const sequence = BigInt(left.orderKey.sourceSequence) - BigInt(right.orderKey.sourceSequence);
  if (sequence !== 0n) return sequence < 0n ? -1 : 1;
  return left.orderKey.ordinal - right.orderKey.ordinal;
}

export function transcriptOrderIdentity(block: TranscriptBlock) {
  return `${block.orderKey.sourceSequence}:${block.orderKey.ordinal}`;
}

export function canonicalTranscriptValue(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalTranscriptValue).join(",")}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalTranscriptValue(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

export function cloneTranscriptValue<T>(value: T): T {
  return structuredClone(value);
}

export function validateTranscriptPage(
  value: unknown,
  expected: Readonly<{ sessionId: string }>,
): TranscriptPage {
  if (!isRecord(value) || !exactKeys(value, [
    "schema", "sessionId", "projectionVersion", "projectionGeneration", "sourceHighWater",
    "blocks", "olderCursor", "hasOlder", "resumeCursors",
  ]) || value.schema !== PAGE_SCHEMA || value.sessionId !== expected.sessionId
    || value.projectionVersion !== PROJECTION_VERSION || !identifier(value.projectionGeneration)
    || !decimal(value.sourceHighWater) || !Array.isArray(value.blocks)
    || value.blocks.length > MAX_BLOCKS || !value.blocks.every(validBlock)
    || typeof value.hasOlder !== "boolean"
    || (value.olderCursor !== null && !identifier(value.olderCursor))
    || value.hasOlder !== (value.olderCursor !== null)
    || !Array.isArray(value.resumeCursors)
    || serializedBytes(value) > MAX_PAGE_BYTES) invalid("page");
  let previous: TranscriptBlock | null = null;
  const blockIds = new Set<string>();
  let inlineBytes = 0;
  for (const item of value.blocks as TranscriptBlock[]) {
    if (BigInt(item.orderKey.sourceSequence) > BigInt(value.sourceHighWater as string)
      || (previous !== null && compareTranscriptBlocks(previous, item) >= 0)
      || blockIds.has(item.blockId)) invalid("page");
    previous = item;
    blockIds.add(item.blockId);
    const body = item.body;
    inlineBytes += body.kind === "tool"
      ? utf8Bytes((body.summary as string | null) || "")
      : utf8Bytes(((body.content as TranscriptTextContent).inlineContent) || "");
  }
  if (inlineBytes > MAX_INLINE_BYTES) invalid("page");
  const cursors = value.resumeCursors;
  if (value.sourceHighWater === "0") {
    if (cursors.length !== 0) invalid("page");
  } else if (cursors.length !== 1 || !isRecord(cursors[0])
    || !exactKeys(cursors[0], ["streamId", "cursor"])
    || cursors[0].streamId !== WORKSPACE_STREAM_ID || !decimal(cursors[0].cursor)
    || BigInt(cursors[0].cursor) > BigInt(value.sourceHighWater as string)) invalid("page");
  return cloneTranscriptValue(value as TranscriptPage);
}

export function validateTranscriptTailPage(value: unknown): TranscriptPage {
  if (!isRecord(value) || !identifier(value.sessionId)) invalid("page");
  return validateTranscriptPage(value, { sessionId: value.sessionId as string });
}

export function validateTranscriptPatchPage(
  value: unknown,
  expected: TranscriptIdentity & Readonly<{ afterSourceHighWater: string }>,
): TranscriptPatchPage {
  if (!isRecord(value) || !exactKeys(value, [
    "schema", "sessionId", "projectionVersion", "projectionGeneration", "throughSourceHighWater",
    "patches", "nextSourceHighWater", "hasMore",
  ]) || value.schema !== PATCH_PAGE_SCHEMA || value.sessionId !== expected.sessionId
    || value.projectionVersion !== expected.projectionVersion
    || value.projectionGeneration !== expected.projectionGeneration
    || !decimal(value.throughSourceHighWater) || !decimal(value.nextSourceHighWater)
    || !decimal(expected.afterSourceHighWater) || typeof value.hasMore !== "boolean"
    || !Array.isArray(value.patches) || value.patches.length > MAX_PATCHES
    || BigInt(value.nextSourceHighWater) < BigInt(expected.afterSourceHighWater)
    || serializedBytes(value) > MAX_PATCH_PAGE_BYTES) invalid("patch page");
  let previous = BigInt(expected.afterSourceHighWater);
  for (const item of value.patches) {
    if (!isRecord(item) || !exactKeys(item, [
      "schema", "sessionId", "projectionVersion", "projectionGeneration", "sourceHighWater",
      "streamId", "appliedCursor", "upserts", "removals",
    ]) || item.schema !== PATCH_SCHEMA || item.sessionId !== expected.sessionId
      || item.projectionVersion !== expected.projectionVersion
      || item.projectionGeneration !== expected.projectionGeneration
      || item.streamId !== WORKSPACE_STREAM_ID || item.appliedCursor !== item.sourceHighWater
      || !decimal(item.sourceHighWater) || BigInt(item.sourceHighWater) <= previous
      || BigInt(item.sourceHighWater) > BigInt(value.throughSourceHighWater)
      || !Array.isArray(item.upserts) || !Array.isArray(item.removals)
      || item.upserts.length + item.removals.length > MAX_PATCH_CHANGES
      || !item.upserts.every(validBlock)
      || !item.removals.every((removal) => isRecord(removal)
        && exactKeys(removal, ["blockId", "blockRevision"])
        && identifier(removal.blockId) && decimal(removal.blockRevision))
      || new Set([
        ...item.upserts.map((block) => block.blockId),
        ...item.removals.map((removal) => removal.blockId),
      ]).size !== item.upserts.length + item.removals.length
      || serializedBytes(item) > MAX_PATCH_BYTES) invalid("patch page");
    previous = BigInt(item.sourceHighWater);
  }
  if ((value.patches.length > 0 && value.nextSourceHighWater !== value.patches.at(-1)?.sourceHighWater)
    || (value.patches.length === 0
      && (value.nextSourceHighWater !== value.throughSourceHighWater
        || expected.afterSourceHighWater !== value.throughSourceHighWater))
    || value.hasMore !== (BigInt(value.nextSourceHighWater) < BigInt(value.throughSourceHighWater))) {
    invalid("patch page");
  }
  return cloneTranscriptValue(value as TranscriptPatchPage);
}

export function parseTranscriptLiveOverlay(value: unknown): TranscriptLiveOverlay | null {
  if (!isRecord(value) || !exactKeys(value, [
    "messageId", "turnId", "afterSourceHighWater", "revision", "text", "reasoning",
  ]) || !identifier(value.messageId) || !identifier(value.turnId)
    || !decimal(value.afterSourceHighWater) || !Number.isSafeInteger(value.revision)
    || Number(value.revision) < 1 || typeof value.text !== "string"
    || (value.reasoning !== null && (!isRecord(value.reasoning)
      || !exactKeys(value.reasoning, ["blockId", "requestId", "text"])
      || !identifier(value.reasoning.blockId) || !identifier(value.reasoning.requestId)
      || typeof value.reasoning.text !== "string"))) return null;
  return cloneTranscriptValue(value as TranscriptLiveOverlay);
}
