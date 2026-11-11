import { AnimatedDisclosure } from "./AnimatedDisclosure";
import { TranscriptSearchQuery, HighlightedTranscriptText } from "./TranscriptSearchHighlight";
import type { CitationSummary } from "./citationSnapshot";
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  Bot,
  Brain,
  ChevronDown,
  CircleAlert,
  FileOutput,
  Globe,
  ListChecks,
  Search,
  SquarePen,
  SquareTerminal,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "../i18n";
import { apiUrl } from "../api";
import { modelFailureDetail } from "../modelErrors";
import { MarkdownContent } from "./MarkdownContent";
import { reasoningPreview } from "./reasoningPreview";
import { useStickToBottom } from "./useStickToBottom";
import { TranscriptContentReader } from "./transcriptContentReader";
import { loadTranscriptContentRange } from "./transcriptContentRanges";
import { TranscriptReferencedContent } from "./TranscriptReferencedContent";
import { TranscriptMessageMeta } from "./TranscriptMessageMeta";
import { toolGroupIconCategory, type ToolGroupCategory } from "./toolGroupTitle";
import type { TranscriptContentRef } from "./transcriptContract";
import type { TranscriptToolOperationRegistry } from "./transcriptToolOperations";
import type { TranscriptLiveOverlay, TranscriptViewStore } from "./transcriptViewStore";


const TOOL_GROUP_ICONS: Record<ToolGroupCategory, LucideIcon> = {
  edit: SquarePen,
  publishArtifact: FileOutput,
  command: SquareTerminal,
  read: Search,
  webSearch: Globe,
  agent: Bot,
  taskOutput: ListChecks,
};

export function TranscriptFailureNotice({ blockId, message }: Readonly<{ blockId?: string; message: string }>) {
  return <div className="workspaceTranscriptBlock workspaceRunError" data-block-id={blockId} role="status">
    <span className="workspaceRunErrorIcon" aria-hidden="true"><CircleAlert /></span>
    <div className="workspaceRunErrorText">{message}</div>
  </div>;
}

function useTranscriptIdentity(store: TranscriptViewStore) {
  const subscribe = useCallback((listener: () => void) => store.subscribeList(listener), [store]);
  const snapshot = useCallback(() => store.getListSnapshot(), [store]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

function useTranscriptBlock(store: TranscriptViewStore, blockId: string) {
  const subscribe = useCallback(
    (listener: () => void) => store.subscribeBlock(blockId, listener),
    [blockId, store],
  );
  const snapshot = useCallback(() => store.getBlockSnapshot(blockId), [blockId, store]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

function useTranscriptViewEpoch(store: TranscriptViewStore) {
  const snapshot = useCallback(() => store.getListSnapshot().viewEpoch, [store]);
  return useSyncExternalStore(store.subscribeList, snapshot, snapshot);
}

function contentText(content: unknown) {
  if (typeof content !== "object" || content === null || !("inlineContent" in content)) return null;
  return typeof content.inlineContent === "string" ? content.inlineContent : null;
}

function contentReference(value: unknown): TranscriptContentRef | null {
  if (typeof value !== "object" || value === null) return null;
  const reference = value as Record<string, unknown>;
  if (typeof reference.refId !== "string"
    || typeof reference.revision !== "string"
    || typeof reference.byteLength !== "string") return null;
  return reference as TranscriptContentRef;
}

function opObject(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? value as Record<string, unknown> : null;
}

function opText(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function textSource(body: Record<string, unknown>) {
  const content = body.content;
  return {
    text: contentText(content),
    reference: contentReference((content as { sourceRef?: unknown } | undefined)?.sourceRef),
  };
}

function ReferencedContent({ store, reference, mode = "markdown", allowVisuals = false }: Readonly<{
  store: TranscriptViewStore; reference: TranscriptContentRef; mode?: "markdown" | "plain"; allowVisuals?: boolean;
}>) {
  const identity = useTranscriptIdentity(store);
  useSyncExternalStore(store.subscribePresentationContent, store.getPresentationContentVersion, store.getPresentationContentVersion);
  const { t } = useTranslation();
  const pushed = store.getPresentationContentSnapshot(reference);
  const live = useSyncExternalStore(store.subscribeLive, store.getLiveSnapshot, store.getLiveSnapshot);
  const sameReference = (blockId: string) => {
    const body = store.getBlockSnapshot(blockId)?.body;
    const candidate = body ? textSource(body).reference : null;
    return candidate?.refId === reference.refId && candidate.revision === reference.revision
      && candidate.byteLength === reference.byteLength;
  };
  const coveredByLive = live !== null && ((Boolean(live.text) && (sameReference(live.messageId)
    || sameReference(store.getLiveTextReplacementBlockId() ?? "")))
    || (Boolean(live.reasoning?.text) && sameReference(live.reasoning?.blockId ?? "")));
  const evicted = store.isPresentationContentEvicted(reference);
  const streamedBody = useRef<HTMLDivElement>(null);
  const retainedHeight = useRef(0);
  useLayoutEffect(() => {
    if (pushed !== null && streamedBody.current) retainedHeight.current = streamedBody.current.getBoundingClientRect().height;
  }, [pushed]);
  if (!identity.sessionId || !identity.projectionGeneration) return null;
  if (pushed !== null && !pushed.complete && coveredByLive) return null;
  if (pushed !== null) return <div ref={streamedBody}>
    {pushed.content ? (mode === "markdown" ? <MarkdownContent text={pushed.content} allowVisuals={allowVisuals} isStreaming={!pushed.complete} /> : <HighlightedTranscriptText text={pushed.content} />) : null}
    {pushed.limited ? <span role="status">{t("transcriptBlockContent.contentPreviewLimited")}</span> : null}
  </div>;
  if (evicted) return <div style={retainedHeight.current ? { minHeight: retainedHeight.current } : undefined}>
    <TranscriptReferencedContent sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration}
      reference={reference} mode={mode} allowVisuals={allowVisuals} deferUntilVisible />
  </div>;
  return <TranscriptReferencedContent
    key={`${identity.viewEpoch}:${identity.sessionId}:${identity.projectionGeneration}:${reference.refId}:${reference.revision}`}
    sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} reference={reference} mode={mode} allowVisuals={allowVisuals}
  />;
}

function TranscriptReasoning({ store, body }: Readonly<{
  store: TranscriptViewStore; body: Record<string, unknown>;
}>) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const bodyRef = useStickToBottom<HTMLDivElement>(expanded);
  const { text, reference } = textSource(body);
  const running = body.status === "queued" || body.status === "running";
  const label = running ? t("reasoningBlock.thinking") : t("reasoningBlock.thoughts");
  const preview = text ? reasoningPreview(text) : "";
  return (
    <div className="workspaceReasoning">
      <button
        type="button"
        className="workspaceActivityGroup"
        aria-label={label}
        aria-expanded={expanded}
        onClick={() => setExpanded((value) => !value)}
      >
        <Brain aria-hidden="true" />
        <span className={`workspaceReasoningLabel${running ? " statusShimmer" : ""}`}>{label}</span>
        <ChevronDown className={`workspaceActivityGroupChevron ${expanded ? "isExpanded" : ""}`} aria-hidden="true" />
        {!expanded && preview ? (
          <span className="reasoningPreview" aria-hidden="true">
            <span className="reasoningPreviewText">{preview}</span>
          </span>
        ) : null}
      </button>
      <AnimatedDisclosure expanded={expanded}>
        <div className="workspaceReasoningBody" role="region" aria-label={t("reasoningBlock.thinkingContent")} tabIndex={0} ref={bodyRef}>
          {text === null
            ? (reference ? <ReferencedContent store={store} reference={reference} mode="markdown" /> : null)
            : <MarkdownContent text={text} />}
        </div>
      </AnimatedDisclosure>
    </div>
  );
}

function TranscriptToolOutput({ store, reference, failed }: Readonly<{
  store: TranscriptViewStore; reference: TranscriptContentRef; failed: boolean;
}>) {
  const identity = useTranscriptIdentity(store);
  const { refId, revision, byteLength } = reference;
  const sessionId = identity.sessionId;
  const projectionGeneration = identity.projectionGeneration;
  const [reader, setReader] = useState<TranscriptContentReader | null>(null);

  useEffect(() => {
    if (!sessionId || !projectionGeneration) return undefined;
    const next = new TranscriptContentReader((offset, signal) =>
      loadTranscriptContentRange({ sessionId, projectionGeneration, reference: { refId, revision, byteLength } }, offset, signal));
    setReader(next);
    void next.loadMore();
    return () => { next.dispose(); };
  }, [sessionId, projectionGeneration, refId, revision, byteLength]);

  if (!reader) return null;
  return <ToolOutputBody reader={reader} failed={failed} />;
}

function ToolOutputBody({ reader, failed }: Readonly<{ reader: TranscriptContentReader; failed: boolean }>) {
  const { t } = useTranslation();
  const state = useSyncExternalStore(reader.subscribe, reader.getSnapshot, reader.getSnapshot);
  const scrollRef = useRef<HTMLPreElement>(null);
  const loadedBytes = state.content.length;

  // Fill the viewport: while the output box is not yet scrollable and more
  // content exists, keep loading instead of waiting for a scroll event.
  useEffect(() => {
    const element = scrollRef.current;
    if (!element || !state.hasMore || state.loading || loadedBytes === 0) return;
    if (element.scrollHeight <= element.clientHeight + 1) void reader.loadMore();
  }, [reader, loadedBytes, state.hasMore, state.loading]);

  function handleScroll() {
    const element = scrollRef.current;
    if (!element || !state.hasMore || state.loading) return;
    if (element.scrollHeight - element.clientHeight - element.scrollTop <= 24) void reader.loadMore();
  }

  return (
    <div className="workspaceToolOutput">
      <pre
        ref={scrollRef}
        className={`workspaceToolOutputScroll${failed ? " isError" : ""}`}
        onScroll={handleScroll}
      >{state.content}</pre>
      {state.unavailable ? <span className="workspaceToolOutputStatus isError" role="alert">
        {t("transcriptBlockContent.unableToLoadContent")}
      </span> : null}
    </div>
  );
}

function operationInlineSummary(operation: Record<string, unknown>, call: Record<string, unknown>) {
  const toolName = opText(operation.toolName) || opText(call.toolName);
  const input = opObject(call.normalizedInput);
  const description = opText(input?.description);
  const command = opText(input?.command);
  const target = opText(operation.path) || opText(input?.path)
    || opText(operation.query)
    || opText(operation.text)
    || opText(call.displayTarget);
  return [command, target || opText(input?.path), description, toolName].find((value) => value.length > 0) || toolName;
}

type ToolDisplayStatus = "running" | "completed" | "failed" | "interrupted" | "unconfirmed";

function TranscriptToolNode({ store, call, operation, outputRef, status }: Readonly<{
  store: TranscriptViewStore;
  call: Record<string, unknown>;
  operation: Record<string, unknown>;
  outputRef: TranscriptContentRef | null;
  status: ToolDisplayStatus;
}>) {
  const { t } = useTranslation();
  const failed = status === "failed" || status === "interrupted";
  const [expanded, setExpanded] = useState(false);
  const input = opObject(call.normalizedInput);
  const command = opText(input?.command);
  const kind = opText(operation.kind) || opText(operation.toolName) || opText(call.toolName);
  const isCommand = kind === "command" || kind === "bash" || Boolean(command);
  const summary = operationInlineSummary(operation, call);
  const toolName = opText(operation.toolName) || opText(call.toolName);
  const delivery = opText(input?.delivery);
  const executionFile = toolName === "read_material" && delivery === "execution_file";
  const category = toolGroupIconCategory([{ toolName, delivery, description: "", target: "", status: status === "running" ? "running" : failed ? "error" : "completed" }]) ?? "command";
  const ActivityIcon = TOOL_GROUP_ICONS[category];
  const executionFileReady = status === "completed" && !["failed", "denied", "aborted"].includes(opText(operation.resultState));
  const title = executionFile
    ? t(executionFileReady ? "materialTool.executionFileReady" : "materialTool.executionFileAction")
    : `${t(`toolGroup.${category}.${status === "running" ? "running" : "completed"}`)} ${summary}`;
  const diff = opText(operation.diffPreview);
  const text = opText(operation.modelContent) || opText(operation.outputPreview);
  const error = opText(operation.error);
  const hasDetail = Boolean((isCommand && command) || diff || text || error || outputRef);
  const Header = hasDetail ? "button" : "div";
  return (
    <div className={`agent-operation-group agent-tool-node ${status}${failed ? " error" : ""}`}>
      <Header
        className="agent-operation-summary agent-tool-node-summary"
        {...(hasDetail ? { type: "button", "aria-expanded": expanded, onClick: () => setExpanded((value) => !value) } : {})}
      >
        <ActivityIcon className="agent-tool-node-icon" aria-hidden="true" />
        <span className="agent-tool-node-action is-inline-summary" title={title}>{title}</span>
        <span className="agent-tool-node-status">{t(`transcriptTool.status.${status}`)}</span>
        {hasDetail ? <span className={`agent-operation-chevron ${expanded ? "open" : ""}`} aria-hidden="true" /> : null}
      </Header>
      <AnimatedDisclosure expanded={hasDetail && expanded}>
        <div className="agent-operation-body agent-tool-node-body">
          <div className="agent-tool-command-card agent-tool-detail-card">
            {isCommand && command ? <section className="agent-tool-detail-section is-command" aria-label={t("transcriptTool.command")}>
              <div className="agent-tool-detail-section-header"><span className="agent-tool-detail-section-title">{t("transcriptTool.command")}</span></div>
              <div className="agent-tool-command-viewport"><pre className="agent-tool-bash-command">{`$ ${command}`}</pre></div>
            </section> : null}
            {diff ? <section className="agent-tool-detail-section is-diff" aria-label={t("transcriptTool.changes")}>
              <div className="agent-tool-detail-section-header"><span className="agent-tool-detail-section-title">{t("transcriptTool.changes")}</span></div>
              <pre className="agent-tool-output-block is-diff">{diff}</pre>
            </section> : null}
            {text || error || outputRef ? <section className="agent-tool-detail-section is-output" aria-label={t("transcriptTool.output")}>
              <div className="agent-tool-detail-section-header"><span className="agent-tool-detail-section-title">{t("transcriptTool.output")}</span></div>
              {text ? <pre className={`agent-tool-output-block${failed ? " is-error" : ""}`}>{text}</pre> : null}
              {error ? <pre className="agent-tool-output-block is-error" role="alert">{error}</pre> : null}
              {outputRef ? <TranscriptToolOutput store={store} reference={outputRef} failed={failed} /> : null}
            </section> : null}
          </div>
        </div>
      </AnimatedDisclosure>
    </div>
  );
}

function useToolOperationsSnapshot(registry: TranscriptToolOperationRegistry | undefined) {
  const subscribe = useCallback(
    (listener: () => void) => (registry ? registry.subscribe(listener) : () => {}),
    [registry],
  );
  const snapshot = useCallback(
    () => (registry ? registry.getSnapshot() : null),
    [registry],
  );
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

// Consecutive tool blocks share one card (desktop groups consecutive tool
// tasks the same way); each call contributes one or more operation nodes.
export function TranscriptToolGroupCard({ store, blockIds, toolOperations, citations, onShowCitation, runEnded = false }: Readonly<{
  store: TranscriptViewStore;
  blockIds: readonly string[];
  citations?: ReadonlyMap<string, readonly CitationSummary[]>;
  onShowCitation?(citation: CitationSummary, origin: { elementId: string }): void;
  toolOperations?: TranscriptToolOperationRegistry;
  runEnded?: boolean;
}>) {
  const { t } = useTranslation();
  const listIdentity = useTranscriptIdentity(store);
  const listViewEpoch = listIdentity.viewEpoch;
  const registrySnapshot = useToolOperationsSnapshot(toolOperations);
  const managedBytes = useSyncExternalStore(
    store.subscribeManagedContent,
    store.managedContentBytes,
    store.managedContentBytes,
  );
  void listViewEpoch;
  void managedBytes;
  const entries = blockIds
    .map((blockId) => {
      const block = store.getBlockSnapshot(blockId);
      if (!block) return null;
      const body = block.body as Record<string, unknown>;
      const callId = opText(body.callId);
      const record = registrySnapshot ? registrySnapshot.records.get(callId) ?? null : null;
      return {
        body,
        sourceSequence: block.orderKey.sourceSequence,
        callId,
        call: (record?.call ?? {}) as Record<string, unknown>,
        resultState: opText(record?.result?.resultState),
        operations: (record?.operations ?? []) as readonly Record<string, unknown>[],
        outputRef: contentReference(body.outputRef),
        detailSourceEventId: record?.previewTruncated ? opText(record.detailSourceEventId) : "",
        unconfirmed: runEnded && (body.status === "running" || body.status === "queued"),
      };
    })
    .filter((entry): entry is NonNullable<typeof entry> => entry !== null);
  if (entries.length === 0) return null;
  return (
    <div className="workspaceActivityGroupRecord">
      {entries.map(entry => {
        const sources = citations?.get(entry.sourceSequence) ?? [];
        return sources.length ? <div className="workspaceCitationSources" key={entry.sourceSequence}>
          <span>{t("transcriptBlockContent.toolSources")}</span>
          {sources.map(citation => {
            const elementId = `citation-${entry.sourceSequence}-${citation.citationId}`;
            return <button type="button" id={elementId} key={citation.citationId}
              onClick={() => onShowCitation?.(citation, { elementId })}>{citation.displayName}</button>;
          })}
        </div> : null;
      })}
      <div className="agent-tool-node-list">
              {entries.flatMap((entry) => {
                if (entry.operations.length === 0) {
                  return [
                    <TranscriptToolNode
                      store={store}
                      call={{ ...entry.call, toolName: opText(entry.body.toolName) }}
                      operation={{ callId: entry.callId, toolName: opText(entry.body.toolName), text: opText(entry.body.summary), resultState: entry.resultState }}
                      outputRef={entry.outputRef}
                      status={entry.unconfirmed ? "unconfirmed" : (entry.body.status === "queued" ? "running" : entry.body.status) as ToolDisplayStatus}
                      key={entry.callId}
                    />,
                  ];
                }
                return entry.operations.map((operation, index) => (
                  <TranscriptToolNode
                    store={store}
                    call={{ ...entry.call, toolName: opText(entry.body.toolName) }}
                    operation={operation}
                    outputRef={opText(operation.callId) === entry.callId ? entry.outputRef : null}
                    status={entry.unconfirmed ? "unconfirmed" : (entry.body.status === "queued" ? "running" : entry.body.status) as ToolDisplayStatus}
                    key={`${entry.callId}:${opText(operation.toolName)}:${index}`}
                  />
                ));
              })}
      </div>
      {listIdentity.sessionId ? entries.filter(entry => entry.detailSourceEventId).map(entry => <a key={entry.callId}
              href={apiUrl(`/api/sessions/${encodeURIComponent(listIdentity.sessionId as string)}/presentation/operation?sourceEventId=${encodeURIComponent(entry.detailSourceEventId)}`)} download>
              {t("libraryObjectRoute.downloadFile")}
            </a>) : null}
    </div>
  );
}

export const TranscriptBlockRow = memo(function TranscriptBlockRow({
  store,
  blockId,
  searchQuery,
}: Readonly<{
  store: TranscriptViewStore;
  blockId: string;
  searchQuery?: string;
}>) {
  return <TranscriptSearchQuery value={searchQuery ?? ""}><TranscriptBlockBody store={store} blockId={blockId} searchHit={searchQuery !== undefined} /></TranscriptSearchQuery>;
});

function TranscriptBlockBody({ store, blockId, searchHit }: { store: TranscriptViewStore; blockId: string; searchHit: boolean }) {
  const { t } = useTranslation();
  const block = useTranscriptBlock(store, blockId);
  const viewEpoch = useTranscriptViewEpoch(store);
  const identity = store.getListSnapshot();
  if (block === null) return null;
  const body = block.body as Record<string, unknown>;
  const kind = typeof body.kind === "string" ? body.kind : "";
  const { text, reference } = textSource(body);
  const failedRun = block.presentation?.sourceType === "agent_run_failed"
    || block.presentation?.sourceType === "agent_run_interrupted";
  if (kind === "notice" && body.noticeType === "run_boundary" && failedRun) {
    return <TranscriptFailureNotice blockId={block.blockId} message={modelFailureDetail(text, t, block.presentation?.reasonType)} />;
  }
  if (kind === "userText") {
    return (
      <div className="workspaceTranscriptBlock workspaceTranscriptUser" data-block-id={block.blockId} data-search-hit={searchHit || undefined} tabIndex={searchHit ? -1 : undefined}>
        <div className="workspaceUserMessage">
          {text !== null ? <HighlightedTranscriptText text={text} /> : (reference ? <ReferencedContent store={store} reference={reference} mode="plain" /> : null)}
        </div>
        <TranscriptMessageMeta key={`${viewEpoch}:${block.blockRevision}`} user text={text} reference={reference}
          sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} observedAtMs={block.presentation?.observedAtMs} />
      </div>
    );
  }
  if (kind === "assistantText") {
    return (
      <div className="workspaceTranscriptBlock workspaceTranscriptAssistant" data-block-id={block.blockId} data-search-hit={searchHit || undefined} tabIndex={searchHit ? -1 : undefined}>
        <div className="workspaceTerminalAnswer">
          {text === null
            ? (reference ? <ReferencedContent store={store} reference={reference} mode="markdown" allowVisuals /> : null)
            : <MarkdownContent text={text} allowVisuals isStreaming={body.status === "queued" || body.status === "running"} />}
        </div>
        {body.status !== "queued" && body.status !== "running" ? <TranscriptMessageMeta key={`${viewEpoch}:${block.blockRevision}`} answer text={text} reference={reference}
          sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} observedAtMs={block.presentation?.observedAtMs} /> : null}
      </div>
    );
  }
  if (kind === "reasoning") {
    return (
      <div className="workspaceTranscriptBlock" data-block-id={block.blockId}>
        <TranscriptReasoning store={store} body={body} />
      </div>
    );
  }
  if (kind === "notice" && body.noticeType === "turn_supplement") {
    return <div className="workspaceTranscriptBlock workspaceSupplement" data-block-id={block.blockId}>
      {text === null
        ? (reference ? <ReferencedContent store={store} reference={reference} mode="markdown" /> : null)
        : <MarkdownContent text={text} />}
    </div>;
  }
  return (
    <div className="workspaceTranscriptBlock workspaceStageSummary" data-block-id={block.blockId} data-search-hit={searchHit || undefined} tabIndex={searchHit ? -1 : undefined}>
      {text === null
        ? (reference ? <ReferencedContent store={store} reference={reference} mode="markdown" allowVisuals /> : null)
        : <MarkdownContent text={text} allowVisuals />}
      {kind === "notice" && body.noticeType !== "run_boundary" ? <TranscriptMessageMeta key={`${viewEpoch}:${block.blockRevision}`} text={text} reference={reference}
        sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} observedAtMs={block.presentation?.observedAtMs} /> : null}
    </div>
  );
}

export const TranscriptLiveTail = memo(function TranscriptLiveTail({ live, mode = "all" }: Readonly<{
  live: TranscriptLiveOverlay;
  mode?: "all" | "reasoning" | "text";
}>) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const reasoningBodyRef = useStickToBottom<HTMLDivElement>(expanded);
  const reasoningText = mode === "text" ? "" : live.reasoning?.text ?? "";
  const text = mode === "reasoning" ? "" : live.text;
  const preview = reasoningText ? reasoningPreview(reasoningText) : "";
  if (!reasoningText && !text) return null;
  return (
    <div className="workspaceTranscriptLive" data-block-id={`live:${live.messageId}:reasoning`}>
      {reasoningText ? (
        <div className="workspaceReasoning">
          <button
            type="button"
            className="workspaceActivityGroup"
            aria-label={t("reasoningBlock.thinking")}
            aria-expanded={expanded}
            onClick={() => setExpanded((value) => !value)}
          >
            <Brain aria-hidden="true" />
            <span className="workspaceReasoningLabel statusShimmer">{t("reasoningBlock.thinking")}</span>
            <ChevronDown className={`workspaceActivityGroupChevron ${expanded ? "isExpanded" : ""}`} aria-hidden="true" />
            {!expanded && preview ? (
              <span className="reasoningPreview" aria-hidden="true">
                <span className="reasoningPreviewText">{preview}</span>
              </span>
            ) : null}
          </button>
          <AnimatedDisclosure expanded={expanded}>
            <div className="workspaceReasoningBody" role="region" aria-label={t("reasoningBlock.thinkingContent")} tabIndex={0} ref={reasoningBodyRef}>
              <MarkdownContent text={reasoningText} />
            </div>
          </AnimatedDisclosure>
        </div>
      ) : null}
      {text ? <div className="workspaceTranscriptAssistant workspaceTerminalAnswer" data-block-id={`live:${live.messageId}`}>
        <MarkdownContent text={text} allowVisuals isStreaming />
      </div> : null}
    </div>
  );
});
