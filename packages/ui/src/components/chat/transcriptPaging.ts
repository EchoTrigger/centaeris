import {
  getTranscriptPage,
  getTranscriptPatches,
  TRANSCRIPT_PROJECTION_VERSION_V1,
  type TranscriptBlockStatusV1,
  type TranscriptBlockV1,
  type TranscriptContentRefV1,
  type TranscriptPageRpcRequestV1,
  type TranscriptPageRpcResponseV1,
  type TranscriptPageV1,
  type TranscriptPatchRpcRequestV1,
  type TranscriptPatchRpcResponseV1,
  type TranscriptPatchV1,
  type TranscriptTextContentV1,
} from "../../lib/chatBridge";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  TaskStatus,
} from "./types";

const TRANSCRIPT_PAGE_RPC_SCHEMA = "transcript.page.rpc.v1";
const TRANSCRIPT_PATCH_RPC_SCHEMA = "transcript.patch.rpc.v1";
const TRANSCRIPT_PROJECTION_POLL_MS = 50;
const TRANSCRIPT_PROJECTION_STALL_MAX_POLLS = 600;
const MAX_U64 = 18_446_744_073_709_551_615n;
const MAX_U32 = 4_294_967_295;
const MAX_SAFE_UI_INTEGER = BigInt(Number.MAX_SAFE_INTEGER);
const transcriptByteEncoder = new TextEncoder();

type OrderedBlock = {
  block: TranscriptBlockV1;
  sourceSequence: bigint;
};

const waitForProjection = async (): Promise<void> => {
  await new Promise<void>((resolve) => {
    window.setTimeout(resolve, TRANSCRIPT_PROJECTION_POLL_MS);
  });
};

const requireIdentifier = (value: string, field: string): void => {
  if (!value.trim()) {
    throw new Error(`${field} must not be empty`);
  }
};

const parseDecimal = (value: string, field: string): bigint => {
  if (!/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new Error(`${field} must be a canonical decimal string`);
  }
  const parsed = BigInt(value);
  if (parsed > MAX_U64) {
    throw new Error(`${field} exceeds the u64 range`);
  }
  return parsed;
};

const validateContentRef = (
  reference: TranscriptContentRefV1,
  field: string,
): void => {
  requireIdentifier(reference.refId, `${field}.refId`);
  parseDecimal(reference.revision, `${field}.revision`);
  parseDecimal(reference.byteLength, `${field}.byteLength`);
};

const validateTextContent = (
  content: TranscriptTextContentV1,
  field: string,
): void => {
  const hasInline = typeof content.inlineContent === "string";
  const hasReference = content.sourceRef !== undefined;
  if (hasInline === hasReference) {
    throw new Error(`${field} must contain exactly one content source`);
  }
  if (content.sourceRef) {
    validateContentRef(content.sourceRef, `${field}.sourceRef`);
  }
};

const validateStatus = (
  status: TranscriptBlockStatusV1,
  field: string,
): void => {
  if (
    status !== "queued" &&
    status !== "running" &&
    status !== "completed" &&
    status !== "failed" &&
    status !== "interrupted"
  ) {
    throw new Error(`${field} is invalid`);
  }
};

const validateBlock = (block: TranscriptBlockV1): OrderedBlock => {
  requireIdentifier(block.blockId, "transcript blockId");
  parseDecimal(block.blockRevision, "transcript blockRevision");
  const sourceSequence = parseDecimal(
    block.orderKey.sourceSequence,
    "transcript block sourceSequence",
  );
  if (
    !Number.isInteger(block.orderKey.ordinal) ||
    block.orderKey.ordinal < 0 ||
    block.orderKey.ordinal > MAX_U32
  ) {
    throw new Error("transcript block ordinal is invalid");
  }
  const body = block.body;
  switch (body.kind) {
    case "userText":
      validateTextContent(body.content, `transcript ${body.kind} content`);
      break;
    case "assistantText":
      validateTextContent(body.content, `transcript ${body.kind} content`);
      validateStatus(body.status, `transcript ${body.kind} status`);
      break;
    case "reasoning":
      requireIdentifier(body.requestId, "transcript reasoning requestId");
      validateTextContent(body.content, "transcript reasoning content");
      validateStatus(body.status, "transcript reasoning status");
      break;
    case "tool": {
      requireIdentifier(body.callId, "transcript tool callId");
      requireIdentifier(body.toolName, "transcript toolName");
      validateStatus(body.status, "transcript tool status");
      const hasSummary = typeof body.summary === "string";
      const hasSummaryRef = body.summaryRef !== null;
      if (hasSummary === hasSummaryRef) {
        throw new Error(
          "transcript tool must contain exactly one summary source",
        );
      }
      if (body.summaryRef) {
        validateContentRef(body.summaryRef, "transcript tool summaryRef");
      }
      if (body.outputRef) {
        validateContentRef(body.outputRef, "transcript tool outputRef");
      }
      break;
    }
    case "notice":
      requireIdentifier(body.noticeType, "transcript noticeType");
      validateTextContent(body.content, "transcript notice content");
      validateStatus(body.status, "transcript notice status");
      break;
    default:
      body satisfies never;
  }
  return { block, sourceSequence };
};

const validatePage = (page: TranscriptPageV1): void => {
  if (
    page.schema !== "transcript.page.v1" ||
    page.projectionVersion !== TRANSCRIPT_PROJECTION_VERSION_V1
  ) {
    throw new Error("transcript page identity is invalid");
  }
  requireIdentifier(page.sessionId, "transcript page sessionId");
  requireIdentifier(
    page.projectionGeneration,
    "transcript page projectionGeneration",
  );
  const highWater = parseDecimal(
    page.sourceHighWater,
    "transcript page sourceHighWater",
  );
  if (page.hasOlder !== (page.olderCursor !== null)) {
    throw new Error("transcript page hasOlder and olderCursor disagree");
  }
  if (page.olderCursor !== null) {
    requireIdentifier(page.olderCursor, "transcript page olderCursor");
  }
  const blockIds = new Set<string>();
  let previousOrder: [bigint, number] | null = null;
  for (const block of page.blocks) {
    const ordered = validateBlock(block);
    if (ordered.sourceSequence > highWater) {
      throw new Error("transcript page block is after sourceHighWater");
    }
    if (blockIds.has(block.blockId)) {
      throw new Error("transcript page contains duplicate blockId");
    }
    blockIds.add(block.blockId);
    const currentOrder: [bigint, number] = [
      ordered.sourceSequence,
      block.orderKey.ordinal,
    ];
    if (
      previousOrder &&
      (previousOrder[0] > currentOrder[0] ||
        (previousOrder[0] === currentOrder[0] &&
          previousOrder[1] >= currentOrder[1]))
    ) {
      throw new Error("transcript page blocks are not strictly ordered");
    }
    previousOrder = currentOrder;
  }
};

const validatePatch = (patch: TranscriptPatchV1): void => {
  if (
    patch.schema !== "transcript.patch.v1" ||
    patch.projectionVersion !== TRANSCRIPT_PROJECTION_VERSION_V1
  ) {
    throw new Error("transcript patch identity is invalid");
  }
  requireIdentifier(patch.sessionId, "transcript patch sessionId");
  requireIdentifier(
    patch.projectionGeneration,
    "transcript patch projectionGeneration",
  );
  const highWater = parseDecimal(
    patch.sourceHighWater,
    "transcript patch sourceHighWater",
  );
  requireIdentifier(patch.streamId, "transcript patch streamId");
  requireIdentifier(patch.appliedCursor, "transcript patch appliedCursor");
  const changedIds = new Set<string>();
  for (const block of patch.upserts) {
    const ordered = validateBlock(block);
    if (ordered.sourceSequence > highWater) {
      throw new Error("transcript patch block is after sourceHighWater");
    }
    if (changedIds.has(block.blockId)) {
      throw new Error("transcript patch contains duplicate block change");
    }
    changedIds.add(block.blockId);
  }
  for (const removal of patch.removals) {
    requireIdentifier(removal.blockId, "transcript removal blockId");
    parseDecimal(removal.blockRevision, "transcript removal blockRevision");
    if (changedIds.has(removal.blockId)) {
      throw new Error("transcript patch contains duplicate block change");
    }
    changedIds.add(removal.blockId);
  }
};

const referencedContentLabel = (reference: TranscriptContentRefV1): string =>
  `[referenced content: ${reference.byteLength} bytes]`;

const materializeText = (content: TranscriptTextContentV1): string =>
  content.inlineContent ?? referencedContentLabel(content.sourceRef);

const isLiveStatus = (status: TranscriptBlockStatusV1): boolean =>
  status === "queued" || status === "running";

const taskStatus = (status: TranscriptBlockStatusV1): TaskStatus => {
  switch (status) {
    case "queued":
    case "running":
      return "running";
    case "completed":
      return "done";
    case "failed":
    case "interrupted":
      return "error";
  }
};

const safeUiInteger = (value: string, field: string): number => {
  const parsed = parseDecimal(value, field);
  if (parsed > MAX_SAFE_UI_INTEGER) {
    throw new Error(`${field} exceeds the exact UI integer range`);
  }
  return Number(parsed);
};

const emptyTurn = (
  id: string,
  isStreaming: boolean,
): AssistantExecutionTurn => ({
  id,
  chunks: [],
  finalAnswer: "",
  isStreaming,
});

const materializeBlock = (
  block: TranscriptBlockV1,
  sessionId: string,
  projectionGeneration: string,
): ChatMessage => {
  const body = block.body;
  if (body.kind === "userText") {
    return {
      id: block.blockId,
      role: "user",
      text: materializeText(body.content),
    };
  }
  const turn = emptyTurn(block.blockId, isLiveStatus(body.status));
  switch (body.kind) {
    case "assistantText":
      turn.finalAnswer = materializeText(body.content);
      break;
    case "reasoning":
      turn.chunks.push({
        id: block.blockId,
        kind: "reasoning",
        text: materializeText(body.content),
        status:
          body.status === "interrupted"
            ? "interrupted"
            : isLiveStatus(body.status)
              ? "streaming"
              : "done",
      });
      break;
    case "tool": {
      const summary =
        body.summary ??
        (body.summaryRef
          ? referencedContentLabel(body.summaryRef)
          : body.toolName);
      turn.chunks.push({
        id: block.blockId,
        kind: "task",
        task: {
          id: body.callId,
          title: body.toolName,
          summary,
          status: taskStatus(body.status),
          provider: "tool",
          outputByteLength: body.outputRef
            ? safeUiInteger(body.outputRef.byteLength, "outputRef.byteLength")
            : undefined,
          transcriptContentRef: body.outputRef ?? undefined,
          transcriptSessionId: body.outputRef ? sessionId : undefined,
          transcriptProjectionGeneration: body.outputRef
            ? projectionGeneration
            : undefined,
          operations: [
            {
              callId: body.callId,
              toolName: body.toolName,
              status: body.status,
            },
          ],
        },
      });
      break;
    }
    case "notice":
      turn.chunks.push({
        id: block.blockId,
        kind: "narrative",
        text: materializeText(body.content),
        tone: body.status === "failed" ? "error" : "normal",
      });
      break;
    default:
      body satisfies never;
  }
  return { id: block.blockId, role: "assistant", turn };
};

const incomingWins = (
  existing: TranscriptBlockV1,
  incoming: TranscriptBlockV1,
): boolean => {
  const existingRevision = parseDecimal(
    existing.blockRevision,
    "transcript existing blockRevision",
  );
  const incomingRevision = parseDecimal(
    incoming.blockRevision,
    "transcript incoming blockRevision",
  );
  if (existingRevision === incomingRevision) {
    if (JSON.stringify(existing) !== JSON.stringify(incoming)) {
      throw new Error("transcript block revision has conflicting content");
    }
    return false;
  }
  return incomingRevision > existingRevision;
};

export class DesktopTranscriptView {
  readonly sessionId: string;
  readonly projectionGeneration: string;
  readonly sourceHighWater: string;
  private currentHighWater: bigint;
  private readonly baseHighWater: bigint;
  private readonly loadedBlockIds = new Set<string>();
  private readonly visibleBlocks = new Map<string, TranscriptBlockV1>();
  private readonly visibleBlockBytes = new Map<string, number>();
  private visibleBytes = 0;
  private readonly pendingOverrides = new Map<string, TranscriptBlockV1>();
  private readonly postBaseOverrideIds = new Set<string>();
  private readonly materializedMessages = new Map<
    string,
    { block: TranscriptBlockV1; message: ChatMessage }
  >();
  private readonly orderByBlockId = new Map<string, string>();
  private readonly blockIdByOrder = new Map<string, string>();
  private readonly tailBlockIds = new Set<string>();
  private readonly tailOlderCursor: string | null;
  private releaseRevision = 0;
  olderCursor: string | null;

  private constructor(page: TranscriptPageV1) {
    this.sessionId = page.sessionId;
    this.projectionGeneration = page.projectionGeneration;
    this.sourceHighWater = page.sourceHighWater;
    this.baseHighWater = parseDecimal(
      page.sourceHighWater,
      "transcript page sourceHighWater",
    );
    this.currentHighWater = this.baseHighWater;
    this.olderCursor = page.olderCursor;
    this.applyPage(page);
    page.blocks.forEach((block) => this.tailBlockIds.add(block.blockId));
    this.tailOlderCursor = page.olderCursor;
  }

  static open(page: TranscriptPageV1): DesktopTranscriptView {
    validatePage(page);
    return new DesktopTranscriptView(page);
  }

  get hasOlder(): boolean {
    return this.olderCursor !== null;
  }

  get currentSourceHighWater(): string {
    return this.currentHighWater.toString();
  }

  get currentReleaseRevision(): number {
    return this.releaseRevision;
  }

  get managedContentBytes(): number {
    return this.visibleBytes;
  }

  releaseLoadedHistory(): void {
    this.releaseRevision += 1;
    for (const blockId of [...this.visibleBlocks.keys()]) {
      const block = this.visibleBlocks.get(blockId);
      const sequence = block
        ? parseDecimal(block.orderKey.sourceSequence, "transcript block sourceSequence")
        : 0n;
      if (!this.tailBlockIds.has(blockId)
        && !this.postBaseOverrideIds.has(blockId)
        && sequence <= this.baseHighWater) {
        this.visibleBytes -= this.visibleBlockBytes.get(blockId) ?? 0;
        this.visibleBlockBytes.delete(blockId);
        this.visibleBlocks.delete(blockId);
        this.loadedBlockIds.delete(blockId);
        this.materializedMessages.delete(blockId);
        const order = this.orderByBlockId.get(blockId);
        if (order !== undefined) this.blockIdByOrder.delete(order);
        this.orderByBlockId.delete(blockId);
      }
    }
    this.olderCursor = this.tailOlderCursor;
  }

  applyOlderPage(page: TranscriptPageV1): void {
    this.applyPage(page);
  }

  applyPatch(patch: TranscriptPatchV1): void {
    validatePatch(patch);
    this.validateIdentity(
      patch.sessionId,
      patch.projectionVersion,
      patch.projectionGeneration,
    );
    const patchHighWater = parseDecimal(
      patch.sourceHighWater,
      "transcript patch sourceHighWater",
    );
    if (patchHighWater < this.currentHighWater) {
      throw new Error("transcript patch sourceHighWater moved backward");
    }
    if (patch.removals.length > 0) {
      throw new Error(
        "transcript block removal requires explicit view invalidation",
      );
    }
    for (const block of patch.upserts) {
      const sourceSequence = this.registerOrder(block);
      if (
        this.loadedBlockIds.has(block.blockId) ||
        sourceSequence > this.baseHighWater
      ) {
        if (sourceSequence <= this.baseHighWater) {
          this.postBaseOverrideIds.add(block.blockId);
        }
        this.loadedBlockIds.add(block.blockId);
        this.mergeBlock(this.visibleBlocks, block);
      } else {
        this.mergeBlock(this.pendingOverrides, block);
      }
    }
    this.currentHighWater = patchHighWater;
  }

  advanceCurrentSourceHighWater(value: string): void {
    const next = parseDecimal(value, "transcript patch nextSourceHighWater");
    if (next < this.currentHighWater) {
      throw new Error("transcript patch waterline moved backward");
    }
    this.currentHighWater = next;
  }

  materializeMessages(liveOverlayActive: boolean): ChatMessage[] {
    return Array.from(this.visibleBlocks.values())
      .map(validateBlock)
      .filter(
        ({ sourceSequence }) =>
          !liveOverlayActive || sourceSequence <= this.baseHighWater,
      )
      .sort((left, right) => {
        if (left.sourceSequence < right.sourceSequence) return -1;
        if (left.sourceSequence > right.sourceSequence) return 1;
        return left.block.orderKey.ordinal - right.block.orderKey.ordinal;
      })
      .map(({ block }) => {
        const cached = this.materializedMessages.get(block.blockId);
        if (cached?.block === block) {
          return cached.message;
        }
        const message = materializeBlock(
          block,
          this.sessionId,
          this.projectionGeneration,
        );
        this.materializedMessages.set(block.blockId, { block, message });
        return message;
      });
  }

  private applyPage(page: TranscriptPageV1): void {
    validatePage(page);
    this.validateIdentity(
      page.sessionId,
      page.projectionVersion,
      page.projectionGeneration,
    );
    if (page.sourceHighWater !== this.sourceHighWater) {
      throw new Error(
        "transcript history page waterline changed within the view",
      );
    }
    for (const block of page.blocks) {
      this.registerOrder(block);
      this.loadedBlockIds.add(block.blockId);
      this.mergeBlock(this.visibleBlocks, block);
      const override = this.pendingOverrides.get(block.blockId);
      if (override) {
        this.pendingOverrides.delete(block.blockId);
        this.mergeBlock(this.visibleBlocks, override);
      }
    }
    this.olderCursor = page.olderCursor;
  }

  private validateIdentity(
    sessionId: string,
    projectionVersion: string,
    projectionGeneration: string,
  ): void {
    if (
      sessionId !== this.sessionId ||
      projectionVersion !== TRANSCRIPT_PROJECTION_VERSION_V1 ||
      projectionGeneration !== this.projectionGeneration
    ) {
      throw new Error("transcript view identity mismatch");
    }
  }

  private registerOrder(block: TranscriptBlockV1): bigint {
    const sourceSequence = parseDecimal(
      block.orderKey.sourceSequence,
      "transcript block sourceSequence",
    );
    const order = `${block.orderKey.sourceSequence}:${block.orderKey.ordinal}`;
    const existingOrder = this.orderByBlockId.get(block.blockId);
    if (existingOrder && existingOrder !== order) {
      throw new Error("transcript block orderKey changed within the view");
    }
    const existingBlockId = this.blockIdByOrder.get(order);
    if (existingBlockId && existingBlockId !== block.blockId) {
      throw new Error("transcript orderKey is bound to another block in the view");
    }
    this.orderByBlockId.set(block.blockId, order);
    this.blockIdByOrder.set(order, block.blockId);
    return sourceSequence;
  }

  private mergeBlock(
    target: Map<string, TranscriptBlockV1>,
    block: TranscriptBlockV1,
  ): void {
    const existing = target.get(block.blockId);
    if (existing && !incomingWins(existing, block)) {
      return;
    }
    if (target === this.visibleBlocks) {
      this.visibleBytes -= this.visibleBlockBytes.get(block.blockId) ?? 0;
      const bytes = transcriptByteEncoder.encode(JSON.stringify(block)).byteLength;
      this.visibleBlockBytes.set(block.blockId, bytes);
      this.visibleBytes += bytes;
    }
    target.set(block.blockId, block);
  }
}

const validatePageResponse = (
  response: TranscriptPageRpcResponseV1,
): void => {
  if (
    response.schema !== TRANSCRIPT_PAGE_RPC_SCHEMA ||
    response.projectionVersion !== TRANSCRIPT_PROJECTION_VERSION_V1
  ) {
    throw new Error("transcript/page response identity is invalid");
  }
  requireIdentifier(
    response.projectionGeneration,
    "transcript/page projectionGeneration",
  );
};

export const loadTranscriptPageWith = async (
  request: TranscriptPageRpcRequestV1,
  requestPage: (
    request: TranscriptPageRpcRequestV1,
  ) => Promise<TranscriptPageRpcResponseV1>,
  wait: () => Promise<void>,
): Promise<TranscriptPageV1> => {
  requireIdentifier(request.sessionId, "transcript/page sessionId");
  let fixedGeneration = request.projectionGeneration;
  let fixedHighWater = request.sourceHighWater;
  let lastProjected: bigint | null = null;
  let stalledPolls = 0;
  for (;;) {
    const response = await requestPage({
      sessionId: request.sessionId,
      ...(fixedGeneration
        ? { projectionGeneration: fixedGeneration }
        : {}),
      ...(fixedHighWater ? { sourceHighWater: fixedHighWater } : {}),
      ...(request.olderCursor ? { olderCursor: request.olderCursor } : {}),
    });
    validatePageResponse(response);
    const projected = parseDecimal(
      response.projectedSourceHighWater,
      "transcript/page projectedSourceHighWater",
    );
    const target = parseDecimal(
      response.targetSourceHighWater,
      "transcript/page targetSourceHighWater",
    );
    if (projected > target) {
      throw new Error("transcript/page projected waterline exceeds target");
    }
    if (
      (fixedGeneration && fixedGeneration !== response.projectionGeneration) ||
      (fixedHighWater && fixedHighWater !== response.targetSourceHighWater)
    ) {
      throw new Error("transcript/page view identity changed while polling");
    }
    fixedGeneration ??= response.projectionGeneration;
    fixedHighWater ??= response.targetSourceHighWater;
    if (response.targetReached) {
      if (!response.page) {
        throw new Error("transcript/page reached target without a page");
      }
      validatePage(response.page);
      if (
        response.page.sessionId !== request.sessionId ||
        response.page.projectionGeneration !== response.projectionGeneration ||
        response.page.sourceHighWater !== response.targetSourceHighWater
      ) {
        throw new Error("transcript/page payload identity mismatch");
      }
      return response.page;
    }
    if (response.page) {
      throw new Error("transcript/page returned a page before reaching target");
    }
    if (lastProjected === projected) {
      stalledPolls += 1;
      if (stalledPolls >= TRANSCRIPT_PROJECTION_STALL_MAX_POLLS) {
        throw new Error("transcript/page projection did not make progress");
      }
    } else {
      lastProjected = projected;
      stalledPolls = 0;
    }
    await wait();
  }
};

export const loadTranscriptPage = async (
  request: TranscriptPageRpcRequestV1,
): Promise<TranscriptPageV1> =>
  loadTranscriptPageWith(request, getTranscriptPage, waitForProjection);

export const catchUpTranscriptPatchesWith = async (
  view: DesktopTranscriptView,
  requestPatches: (
    request: TranscriptPatchRpcRequestV1,
  ) => Promise<TranscriptPatchRpcResponseV1>,
  wait: () => Promise<void>,
): Promise<boolean> => {
  let targetHighWater: string | undefined;
  let lastProgress: string | null = null;
  let stalledPolls = 0;
  let changed = false;
  for (;;) {
    const afterHighWater = view.currentSourceHighWater;
    const response = await requestPatches({
      sessionId: view.sessionId,
      projectionGeneration: view.projectionGeneration,
      afterSourceHighWater: afterHighWater,
      ...(targetHighWater ? { throughSourceHighWater: targetHighWater } : {}),
    });
    if (
      response.schema !== TRANSCRIPT_PATCH_RPC_SCHEMA ||
      response.projectionVersion !== TRANSCRIPT_PROJECTION_VERSION_V1 ||
      response.projectionGeneration !== view.projectionGeneration
    ) {
      throw new Error("transcript/patches response identity is invalid");
    }
    const projected = parseDecimal(
      response.projectedSourceHighWater,
      "transcript/patches projectedSourceHighWater",
    );
    const responseTarget = parseDecimal(
      response.targetSourceHighWater,
      "transcript/patches targetSourceHighWater",
    );
    const next = parseDecimal(
      response.nextSourceHighWater,
      "transcript/patches nextSourceHighWater",
    );
    const after = parseDecimal(
      afterHighWater,
      "transcript/patches afterSourceHighWater",
    );
    if (targetHighWater && targetHighWater !== response.targetSourceHighWater) {
      throw new Error("transcript/patches target changed while polling");
    }
    targetHighWater ??= response.targetSourceHighWater;
    if (projected > responseTarget || next < after || next > projected) {
      throw new Error("transcript/patches returned invalid waterline progress");
    }
    for (const patch of response.patches) {
      const patchHighWater = parseDecimal(
        patch.sourceHighWater,
        "transcript patch sourceHighWater",
      );
      if (patchHighWater <= after || patchHighWater > next) {
        throw new Error(
          "transcript/patches returned a patch outside the response range",
        );
      }
      view.applyPatch(patch);
      changed = true;
    }
    view.advanceCurrentSourceHighWater(response.nextSourceHighWater);
    if (response.targetReached && !response.hasMore) {
      if (next !== responseTarget) {
        throw new Error(
          "transcript/patches reached target without consuming it",
        );
      }
      return changed;
    }
    const progress = `${projected}:${next}`;
    if (lastProgress === progress) {
      stalledPolls += 1;
      if (stalledPolls >= TRANSCRIPT_PROJECTION_STALL_MAX_POLLS) {
        throw new Error("transcript/patches projection did not make progress");
      }
    } else {
      lastProgress = progress;
      stalledPolls = 0;
    }
    await wait();
  }
};

export const catchUpTranscriptPatches = async (
  view: DesktopTranscriptView,
): Promise<boolean> =>
  catchUpTranscriptPatchesWith(
    view,
    getTranscriptPatches,
    waitForProjection,
  );
