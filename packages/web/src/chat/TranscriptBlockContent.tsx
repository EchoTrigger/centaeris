import { AnimatedDisclosure } from "./AnimatedDisclosure";
import type { CitationSummary } from "./citationSnapshot";
import {
  memo,
  useCallback,
  useEffect,
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
import { modelFailureDetail } from "../modelErrors";
import { MarkdownContent } from "./MarkdownContent";
import { reasoningPreview } from "./reasoningPreview";
import { useStickToBottom } from "./useStickToBottom";
import { TranscriptContentReader } from "./transcriptContentReader";
import { loadTranscriptContentRange } from "./transcriptContentRanges";
import { TranscriptReferencedContent } from "./TranscriptReferencedContent";
import { TranscriptMessageMeta } from "./TranscriptMessageMeta";
import { formatToolGroupTitle, toolGroupIconCategory, type ToolGroupCategory } from "./toolGroupTitle";
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

function ReferencedContent({ store, reference, mode = "markdown" }: Readonly<{
  store: TranscriptViewStore; reference: TranscriptContentRef; mode?: "markdown" | "plain";
}>) {
  const identity = useTranscriptIdentity(store);
  if (!identity.sessionId || !identity.projectionGeneration) return null;
  return <TranscriptReferencedContent
    key={`${identity.viewEpoch}:${identity.sessionId}:${identity.projectionGeneration}:${reference.refId}:${reference.revision}`}
    sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} reference={reference} mode={mode}
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
        <span className={running ? "statusShimmer" : undefined}>{label}</span>
        {!expanded && preview ? (
          <span className="reasoningPreview" aria-hidden="true">
            <span className="reasoningPreviewText">{preview}</span>
          </span>
        ) : null}
        <ChevronDown className={`workspaceActivityGroupChevron ${expanded ? "isExpanded" : ""}`} aria-hidden="true" />
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
  const target = opText(operation.path)
    || opText(operation.query)
    || opText(operation.text)
    || opText(call.displayTarget);
  return [description || command, target, toolName].find((value) => value.length > 0) || toolName;
}

function TranscriptToolNode({ store, call, operation, outputRef, failed }: Readonly<{
  store: TranscriptViewStore;
  call: Record<string, unknown>;
  operation: Record<string, unknown>;
  outputRef: TranscriptContentRef | null;
  failed: boolean;
}>) {
  const [expanded, setExpanded] = useState(false);
  const input = opObject(call.normalizedInput);
  const command = opText(input?.command);
  const kind = opText(operation.kind) || opText(operation.toolName) || opText(call.toolName);
  const isCommand = kind === "command" || kind === "bash";
  const summary = operationInlineSummary(operation, call);
  const diff = opText(operation.diffPreview);
  const text = opText(operation.modelContent) || opText(operation.outputPreview) || opText(operation.error);
  const hasDetail = Boolean((isCommand && command) || diff || text || outputRef);
  const Header = hasDetail ? "button" : "div";
  return (
    <div className={`agent-operation-group agent-tool-node${failed ? " error" : ""}`}>
      <Header
        className="agent-operation-summary agent-tool-node-summary"
        {...(hasDetail ? { type: "button", "aria-expanded": expanded, onClick: () => setExpanded((value) => !value) } : {})}
      >
        <span className="agent-tool-node-action is-inline-summary" title={summary}>{summary}</span>
        {hasDetail ? <span className={`agent-operation-chevron ${expanded ? "open" : ""}`} aria-hidden="true" /> : null}
      </Header>
      <AnimatedDisclosure expanded={hasDetail && expanded}>
        <div className="agent-operation-body agent-tool-node-body">
          <div className="agent-tool-command-card">
            {isCommand && command ? <pre className="agent-tool-bash-command">{`$ ${command}`}</pre> : null}
            {diff ? <pre className="agent-tool-output-block is-diff">{diff}</pre> : null}
            {text ? <pre className={`agent-tool-output-block${failed ? " is-error" : ""}`}>{text}</pre> : null}
            {outputRef
              ? <TranscriptToolOutput store={store} reference={outputRef} failed={failed} />
              : null}
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
  const [expanded, setExpanded] = useState(false);
  const listViewEpoch = useTranscriptIdentity(store).viewEpoch;
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
        operations: (record?.operations ?? []) as readonly Record<string, unknown>[],
        outputRef: contentReference(body.outputRef),
        failed: body.status === "failed" || body.status === "interrupted",
        unconfirmed: runEnded && (body.status === "running" || body.status === "queued"),
      };
    })
    .filter((entry): entry is NonNullable<typeof entry> => entry !== null);
  if (entries.length === 0) return null;
  const operations = entries.flatMap((entry) => {
    const description = opText(opObject(entry.call.normalizedInput)?.description);
    const callTarget = opText(opObject(entry.call.normalizedInput)?.path)
      || opText(entry.body.summary)
      || opText(entry.call.displayTarget);
    const status = entry.failed || entry.unconfirmed
      ? ("error" as const)
      : entry.body.status === "running" || entry.body.status === "queued"
        ? ("running" as const)
        : ("completed" as const);
    if (entry.operations.length === 0) {
      return [{ toolName: opText(entry.body.toolName), description, target: callTarget, status }];
    }
    return entry.operations.map((operation) => ({
      toolName: opText(operation.toolName) || opText(entry.body.toolName),
      description,
      target: opText(operation.path) || opText(operation.query) || opText(operation.text) || callTarget,
      status,
    }));
  });
  const unconfirmed = entries.filter(entry => entry.unconfirmed).length;
  const title = unconfirmed > 0
    ? t("transcriptTool.resultsUnconfirmed", { count: unconfirmed })
    : formatToolGroupTitle(operations, t);
  const GroupIcon = TOOL_GROUP_ICONS[toolGroupIconCategory(operations) ?? "command"];
  const hasDetail = entries.some((entry) => entry.operations.length > 0 || entry.outputRef !== null);
  const Header = hasDetail ? "button" : "div";
  return (
    <div className="workspaceActivityGroupRecord">
      <Header
        className="workspaceActivityGroup"
        {...(hasDetail ? { type: "button", "aria-expanded": expanded, onClick: () => setExpanded((value) => !value) } : {})}
      >
        <GroupIcon aria-hidden="true" />
        <span title={title}>{title}</span>
        {hasDetail ? <ChevronDown className={`workspaceActivityGroupChevron ${expanded ? "isExpanded" : ""}`} aria-hidden="true" /> : null}
      </Header>
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
      <AnimatedDisclosure expanded={hasDetail && expanded}>
        <div className="workspaceActivityDetails">
          <div className="workspaceActivityDetailsInner">
            <div className="agent-tool-node-list">
              {entries.flatMap((entry) => {
                if (entry.operations.length === 0) {
                  return [
                    <TranscriptToolNode
                      store={store}
                      call={entry.call}
                      operation={{ callId: entry.callId, toolName: opText(entry.body.toolName), text: opText(entry.body.summary) }}
                      outputRef={entry.outputRef}
                      failed={entry.failed}
                      key={entry.callId}
                    />,
                  ];
                }
                return entry.operations.map((operation, index) => (
                  <TranscriptToolNode
                    store={store}
                    call={entry.call}
                    operation={operation}
                    outputRef={opText(operation.callId) === entry.callId ? entry.outputRef : null}
                    failed={entry.failed}
                    key={`${entry.callId}:${opText(operation.toolName)}:${index}`}
                  />
                ));
              })}
            </div>
          </div>
        </div>
      </AnimatedDisclosure>
    </div>
  );
}

export type TranscriptInitialInputOrigin = Readonly<{ messageId: string; agentId: string; agentName: string }>;

export const TranscriptBlockRow = memo(function TranscriptBlockRow({
  store,
  blockId,
  initialInputOrigin,
}: Readonly<{
  store: TranscriptViewStore;
  blockId: string;
  initialInputOrigin?: TranscriptInitialInputOrigin | null;
}>) {
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
    return (
      <div className="workspaceTranscriptBlock workspaceRunError" data-block-id={block.blockId} role="status">
        <span className="workspaceRunErrorIcon" aria-hidden="true"><CircleAlert /></span>
        <div className="workspaceRunErrorText">
          {text !== null ? modelFailureDetail(text, t)
            : reference ? <ReferencedContent store={store} reference={reference} mode="plain" /> : null}
        </div>
      </div>
    );
  }
  if (kind === "userText") {
    return (
      <div className="workspaceTranscriptBlock workspaceTranscriptUser" data-block-id={block.blockId}>
        {initialInputOrigin?.messageId === block.blockId ? <span className="workspaceUserMessageOrigin">{t("transcript.sendByAgent", { agentName: initialInputOrigin.agentName })}</span> : null}
        <div className="workspaceUserMessage">
          {text ?? (reference ? <ReferencedContent store={store} reference={reference} mode="plain" /> : null)}
        </div>
        <TranscriptMessageMeta key={`${viewEpoch}:${block.blockRevision}`} user text={text} reference={reference}
          sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} observedAtMs={block.presentation?.observedAtMs} />
      </div>
    );
  }
  if (kind === "assistantText") {
    return (
      <div className="workspaceTranscriptBlock workspaceTranscriptAssistant" data-block-id={block.blockId}>
        <div className="workspaceTerminalAnswer">
          {text === null
            ? (reference ? <ReferencedContent store={store} reference={reference} mode="markdown" /> : null)
            : <MarkdownContent text={text} />}
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
    <div className="workspaceTranscriptBlock workspaceStageSummary" data-block-id={block.blockId}>
      {text === null
        ? (reference ? <ReferencedContent store={store} reference={reference} mode="markdown" /> : null)
        : <MarkdownContent text={text} />}
      {kind === "notice" && body.noticeType !== "run_boundary" ? <TranscriptMessageMeta key={`${viewEpoch}:${block.blockRevision}`} text={text} reference={reference}
        sessionId={identity.sessionId} projectionGeneration={identity.projectionGeneration} observedAtMs={block.presentation?.observedAtMs} /> : null}
    </div>
  );
});

export const TranscriptLiveTail = memo(function TranscriptLiveTail({ live }: Readonly<{
  live: TranscriptLiveOverlay;
}>) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const reasoningBodyRef = useStickToBottom<HTMLDivElement>(expanded);
  const reasoningText = live.reasoning?.text ?? "";
  const preview = reasoningText ? reasoningPreview(reasoningText) : "";
  if (!reasoningText) return null;
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
            <span className="statusShimmer">{t("reasoningBlock.thinking")}</span>
            {!expanded && preview ? (
              <span className="reasoningPreview" aria-hidden="true">
                <span className="reasoningPreviewText">{preview}</span>
              </span>
            ) : null}
            <ChevronDown className={`workspaceActivityGroupChevron ${expanded ? "isExpanded" : ""}`} aria-hidden="true" />
          </button>
          <AnimatedDisclosure expanded={expanded}>
            <div className="workspaceReasoningBody" role="region" aria-label={t("reasoningBlock.thinkingContent")} tabIndex={0} ref={reasoningBodyRef}>
              <MarkdownContent text={reasoningText} />
            </div>
          </AnimatedDisclosure>
        </div>
      ) : null}
    </div>
  );
});
