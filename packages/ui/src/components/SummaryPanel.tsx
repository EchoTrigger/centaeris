import { WorkspaceAddMenu } from "./WorkspaceAddMenu";
import type { TerminalSnapshot } from "../lib/terminalBridge";
import { t } from "../i18n";
import { lazy, Suspense, useMemo, useState } from "react";
import {
  TerminalSquare,
  RefreshCw,
  FileText,
  ListCollapse,
  FileDiff,
  FolderOpen,
  Bot,
  ExternalLink,
  PanelRight,
  Plus,
  X,
} from "lucide-react";
import { Button } from "./ui/button";
import { Tooltip } from "./ui/tooltip";
import type { DiffPanelData, DiffPanelFile } from "../lib/diffPanel";
import type { FilePreviewContentKind } from "../lib/workspaceBridge";
import { renderMarkdownNodes } from "./MarkdownRenderer";
import { WorkspaceFilesPanel } from "./WorkspaceFilesPanel";
import { WorkspaceTasks } from "./WorkspaceTasks";
import { WorkspaceReview } from "./WorkspaceReview";
import { AgentSessionPreview } from "./chat/AgentSessionPreview";

export type SummaryPanelTab = {
  id: string;
  title: string;
  terminalId?: string;
  serviceInstanceId?: string;
  kind: "terminal" | "summary" | "file" | "diffs" | "agent" | "files" | "review" | "tasks";
  workspaceRoot?: string;
  isPreview?: boolean;
  sessionId?: string;
  parentSessionId?: string;
  parentTitle?: string;
  path?: string;
  content?: string;
  contentKind?: FilePreviewContentKind;
  mimeType?: string;
  dataUrl?: string;
  byteLen?: number;
  targetLine?: number;
  targetEndLine?: number;
  loading?: boolean;
  error?: string;
  diffPanel?: DiffPanelData;
};

const WorkspaceTerminal = lazy(() => import("./WorkspaceTerminal"));
const CodePreview = lazy(() => import("./CodePreview"));

type OpenWorkspacePathOptions = {
  taskId?: string;
};

type SummaryPanelProps = {
  workspaceRoot?: string | null;
  sessionId?: string | null;
  onFiles?: () => void;
  onReview?: () => void;
  onTerminal?: (terminal:TerminalSnapshot, serviceInstanceId:string, title?:string) => void;
  visible?: boolean;
  onRefreshFile?: () => void;
  tabs: SummaryPanelTab[];
  activeTabId: string | null;
  onSelectTab: (tabId: string) => void;
  onCloseTab: (tabId: string) => void;
  onAddSummaryTab?: () => void;
  onCollapse?: () => void;
  showTabStrip?: boolean;
  onOpenWorkspacePath?: (path: string, options?: OpenWorkspacePathOptions) => void;
};

const getActiveTab = (
  tabs: SummaryPanelTab[],
  activeTabId: string | null,
): SummaryPanelTab | null => {
  if (!activeTabId) {
    return tabs[0] ?? null;
  }
  return tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null;
};

const isMarkdownPath = (path: string | undefined): boolean => {
  const normalizedPath = (path || "").toLowerCase();
  return (
    normalizedPath.endsWith(".md") ||
    normalizedPath.endsWith(".markdown") ||
    normalizedPath.endsWith(".mdx")
  );
};

function MarkdownPreview({ text }: { text: string }) {
  if (!text.trim()) {
    return <div className="summaryPanelHint">{t("summaryPanel.theFileIsEmpty")}</div>;
  }
  return <div className="summaryMarkdownContent">{renderMarkdownNodes(text)}</div>;
}

function ImagePreview({ tab }: { tab: SummaryPanelTab }) {
  if (!tab.dataUrl) {
    return (
      <div className="summaryPanelHint is-error">
        {t("summaryPanel.imagePreviewIsMissingADataUrl")}
      </div>
    );
  }
  return (
    <div className="summaryImagePreview">
      <div className="summaryImageFrame">
        <img src={tab.dataUrl} alt={tab.title} />
      </div>
      <div className="summaryImageMeta">
        {tab.mimeType ? <span>{tab.mimeType}</span> : null}
        {typeof tab.byteLen === "number" ? <span>{tab.byteLen.toLocaleString()} bytes</span> : null}
      </div>
    </div>
  );
}

function PdfPreview({ tab }: { tab: SummaryPanelTab }) {
  if (!tab.dataUrl) {
    return (
      <div className="summaryPanelHint is-error">
        {t("summaryPanel.pdfPreviewIsMissingADataUrl")}
      </div>
    );
  }
  return (
    <div className="summaryPdfPreview">
      <iframe title={tab.title} src={tab.dataUrl} className="summaryPdfFrame" />
      <div className="summaryImageMeta">
        {tab.mimeType ? <span>{tab.mimeType}</span> : null}
        {typeof tab.byteLen === "number" ? <span>{tab.byteLen.toLocaleString()} bytes</span> : null}
      </div>
    </div>
  );
}

const normalizeDiffCount = (value: number): number => {
  if (!Number.isFinite(value) || value < 0) {
    return 0;
  }
  return Math.floor(value);
};

function DiffStats({
  added,
  removed,
  compact = false,
}: {
  added: number;
  removed: number;
  compact?: boolean;
}) {
  return (
    <span className={`summaryDiffStats ${compact ? "is-compact" : ""}`}>
      <span className="summaryDiffStat is-added">
        +{normalizeDiffCount(added).toLocaleString()}
      </span>
      <span className="summaryDiffStat is-removed">
        -{normalizeDiffCount(removed).toLocaleString()}
      </span>
    </span>
  );
}

function DiffPanelFileRow({
  file,
  isSelected,
  onSelect,
  onOpenWorkspacePath,
}: {
  file: DiffPanelFile;
  isSelected: boolean;
  onSelect: () => void;
  onOpenWorkspacePath?: (path: string, options?: OpenWorkspacePathOptions) => void;
}) {
  return (
    <article className={`summaryDiffFile ${isSelected ? "is-active" : ""}`}>
      <div className="summaryDiffFileSummary">
        <button
          type="button"
          className="summaryDiffFileToggle"
          aria-pressed={isSelected}
          onClick={onSelect}
        >
          <span className="summaryDiffFileIdentity">
            <span className="summaryDiffFileName">{file.title}</span>
            <span className="summaryDiffFilePath">{file.path}</span>
          </span>
        </button>
        {file.diffAvailable === false ? (
          <span className="summaryDiffFileReason">
            {file.diffUnavailableReason || t("summaryPanel.reviewUnavailable")}
          </span>
        ) : (
          <DiffStats added={file.added} removed={file.removed} compact />
        )}
        {onOpenWorkspacePath ? (
          <Tooltip align="end" content={t("summaryPanel.openFile")}>
            <Button
              type="button"
              variant="workspace"
              size="workspaceIcon"
              className="summaryDiffOpenButton"
              aria-label={t("toolActivityTranscript.openValue", { value1: file.path })}
              onClick={() => {
                onOpenWorkspacePath(file.path, { taskId: file.taskId });
              }}
            >
              <ExternalLink className="summaryPanelIcon" aria-hidden="true" />
            </Button>
          </Tooltip>
        ) : null}
      </div>
    </article>
  );
}

function DiffPanelPreview({
  data,
  onOpenWorkspacePath,
}: {
  data: DiffPanelData;
  onOpenWorkspacePath?: (path: string, options?: OpenWorkspacePathOptions) => void;
}) {
  const [selectedFileId, setSelectedFileId] = useState<string | null>(data.files[0]?.id ?? null);
  const selectedFile =
    data.files.find((file) => file.id === selectedFileId) ?? data.files[0] ?? null;

  const totals = useMemo(
    () => ({
      added: normalizeDiffCount(
        data.totalAdded || data.files.reduce((sum, file) => sum + file.added, 0),
      ),
      removed: normalizeDiffCount(
        data.totalRemoved || data.files.reduce((sum, file) => sum + file.removed, 0),
      ),
    }),
    [data.files, data.totalAdded, data.totalRemoved],
  );

  if (data.files.length === 0) {
    return (
      <div className="summaryPanelHint is-error">
        {t("summaryPanel.noFilesToDisplayInTheDiffPanel")}
      </div>
    );
  }

  return (
    <div className="summaryDiffPanel">
      <div className="summaryDiffPanelHeader">
        <div className="summaryDiffPanelHeading">
          <strong>{t("summaryPanel.review")}</strong>
          <span>
            {data.files.length.toLocaleString()} {t("summaryPanel.files")}
          </span>
        </div>
        <DiffStats added={totals.added} removed={totals.removed} />
      </div>
      <div className="summaryDiffPanelContent">
        <div className="summaryDiffPreview">
          {selectedFile?.diffAvailable === false ? (
            <div className="summaryPanelHint">
              {selectedFile.diffUnavailableReason ||
                t("summaryPanel.diffReviewIsUnavailableForThisFile")}
            </div>
          ) : selectedFile ? (
            <Suspense
              fallback={<div className="summaryPanelHint">{t("summaryPanel.loadingDiff")}</div>}
            >
              <CodePreview
                content={selectedFile.diffPreview}
                path={selectedFile.path}
                variant="diff"
              />
            </Suspense>
          ) : null}
        </div>
        <div className="summaryDiffFileList" aria-label={t("summaryPanel.reviewFileList")}>
          {data.files.map((file) => (
            <DiffPanelFileRow
              file={file}
              isSelected={selectedFile?.id === file.id}
              key={file.id}
              onSelect={() => setSelectedFileId(file.id)}
              onOpenWorkspacePath={onOpenWorkspacePath}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

export function SummaryPanel({
  workspaceRoot, sessionId, onFiles, onReview, onTerminal,
  visible = true,
  onRefreshFile,
  tabs,
  activeTabId,
  onSelectTab,
  onCloseTab,
  onAddSummaryTab,
  onCollapse,
  showTabStrip = true,
  onOpenWorkspacePath,
}: SummaryPanelProps) {
  const activeTab = getActiveTab(tabs, activeTabId);
  const browserRoot = tabs.find(
    (tab) => tab.kind === "file" || tab.kind === "files",
  )?.workspaceRoot;
  const showFileBrowser = activeTab?.kind === "file" || activeTab?.kind === "files";

  return (
    <section className="summaryPanel" aria-label={t("summaryPanel.rightPanel")}>
      {showTabStrip ? (
        <div className="summaryPanelTabStrip">
          <div className="summaryPanelTabs" role="tablist" aria-label="Open workspace content">
            {tabs.map((tab, index) => {
              const Icon =
                tab.kind === "terminal" ? TerminalSquare : tab.kind === "tasks" ? ListCollapse : tab.kind === "agent"
                  ? Bot
                  : tab.kind === "files"
                    ? FolderOpen
                    : tab.kind === "diffs" || tab.kind === "review"
                      ? FileDiff
                      : FileText;
              return (
                <div
                  className={`summaryPanelTab ${activeTab?.id === tab.id ? "is-active" : ""} ${tab.isPreview ? "is-preview" : ""}`}
                  key={tab.id}
                >
                  <button
                    type="button"
                    role="tab"
                    id={`tab-${tab.id}`}
                    aria-controls={`panel-${tab.id}`}
                    aria-selected={activeTab?.id === tab.id}
                    tabIndex={activeTab?.id === tab.id ? 0 : -1}
                    className="summaryPanelTabSelect"
                    title={
                      tab.isPreview
                        ? `${tab.path ?? tab.title} — Preview: the next new file replaces this tab`
                        : (tab.path ?? tab.title)
                    }
                    onClick={() => onSelectTab(tab.id)}
                    onKeyDown={(event) => {
                      const target =
                        event.key === "ArrowRight"
                          ? (index + 1) % tabs.length
                          : event.key === "ArrowLeft"
                            ? (index + tabs.length - 1) % tabs.length
                            : event.key === "Home"
                              ? 0
                              : event.key === "End"
                                ? tabs.length - 1
                                : -1;
                      if (target < 0) return;
                      event.preventDefault();
                      onSelectTab(tabs[target].id);
                      event.currentTarget
                        .closest('[role="tablist"]')
                        ?.querySelectorAll<HTMLButtonElement>('[role="tab"]')
                        [target]?.focus();
                    }}
                  >
                    <Icon className="summaryPanelIcon" aria-hidden="true" />
                    <span className="summaryPanelTabTitle">{tab.title}</span>
                  </button>
                  <button
                    type="button"
                    className="summaryPanelTabClose"
                    aria-label={t("summaryPanel.closeValue", { value1: tab.title })}
                    onClick={() => onCloseTab(tab.id)}
                  >
                    <X className="summaryPanelIcon is-close" aria-hidden="true" />
                  </button>
                </div>
              );
            })}
            {onAddSummaryTab ? (
              <Tooltip content={t("summaryPanel.openSummary")}>
                <Button
                  type="button"
                  variant="workspace"
                  size="chromeIcon"
                  className="summaryPanelTabAdd"
                  onClick={onAddSummaryTab}
                  aria-label={t("summaryPanel.openSummary")}
                >
                  <Plus className="summaryPanelIcon" aria-hidden="true" />
                </Button>
              </Tooltip>
            ) : null}
          </div>
          {workspaceRoot && onFiles && onReview && onTerminal ? <WorkspaceAddMenu root={workspaceRoot} sessionId={sessionId ?? null} onFiles={onFiles} onReview={onReview} onTerminal={onTerminal}/> : null}
          {onCollapse ? (
            <Tooltip align="end" content={t("summaryPanel.collapseRightPanel")}>
              <button
                type="button"
                className="summaryPanelCollapse"
                onClick={onCollapse}
                aria-label={t("summaryPanel.collapseRightPanel")}
              >
                <PanelRight className="summaryPanelIcon" aria-hidden="true" />
              </button>
            </Tooltip>
          ) : null}
        </div>
      ) : null}
      <div className="summaryWorkspaceContent">
        <div className="summaryContentViews">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              role="tabpanel"
              id={`panel-${tab.id}`}
              aria-labelledby={`tab-${tab.id}`}
              hidden={activeTab?.id !== tab.id}
              className="summaryContentTab"
            >
              <SummaryTabContent active={visible && activeTab?.id === tab.id} onRefreshFile={onRefreshFile} activeTab={tab} onOpenWorkspacePath={onOpenWorkspacePath} />
            </div>
          ))}
        </div>
        {browserRoot ? (
          <aside
            className="summaryFileBrowser"
            hidden={!showFileBrowser}
            aria-label="Workspace file browser"
          >
            <WorkspaceFilesPanel
              isOpen={visible && showFileBrowser}
              workspaceRoot={browserRoot}
              focusedPath={activeTab?.kind === "file" ? activeTab.path : undefined}
              onOpenFile={(entry) => onOpenWorkspacePath?.(entry.path)}
            />
          </aside>
        ) : null}
      </div>
    </section>
  );
}

function SummaryTabContent({
  active,
  onRefreshFile,
  activeTab,
  onOpenWorkspacePath,
}: {
  active: boolean;
  onRefreshFile?: () => void;
  activeTab: SummaryPanelTab;
  onOpenWorkspacePath: SummaryPanelProps["onOpenWorkspacePath"];
}) {
  const shouldRenderFileAsImage = activeTab?.kind === "file" && activeTab.contentKind === "image";
  const shouldRenderFileAsPdf = activeTab?.kind === "file" && activeTab.contentKind === "pdf";
  const shouldRenderFileAsCode =
    activeTab?.kind === "file" &&
    !shouldRenderFileAsImage &&
    !shouldRenderFileAsPdf &&
    (!isMarkdownPath(activeTab.path) || typeof activeTab.targetLine === "number");

  if(activeTab.kind === "terminal" && activeTab.terminalId && activeTab.serviceInstanceId) return <Suspense fallback={<div>Loading terminal…</div>}><WorkspaceTerminal active={active} target={{terminalId:activeTab.terminalId,serviceInstanceId:activeTab.serviceInstanceId}}/></Suspense>;
  return (
    <>
      {activeTab.kind === "file" || activeTab.kind === "agent" ? (
        <header className="summaryPanelHeader">
          <div className="summaryPanelTitleGroup">
            {activeTab.kind === "agent" ? (
              <h1 className="agentSessionPanelBreadcrumb">
                <span>
                  {activeTab.parentTitle || t("useWorkspacePanelController.mainConversation")}
                </span>
                <span aria-hidden="true">/</span>
                <strong>{activeTab.title}</strong>
              </h1>
            ) : (
              <h1>{activeTab.title}</h1>
            )}
            {activeTab.kind === "file" && activeTab.path ? <span>{activeTab.path}</span> : null}
          </div>
          {activeTab.kind === "file" && onRefreshFile ? <button type="button" aria-label="Refresh file" title="Refresh file" onClick={onRefreshFile}><RefreshCw size={14} aria-hidden="true" /></button> : null}
        </header>
      ) : null}
      <div
        className={`summaryPanelBody ${shouldRenderFileAsCode || shouldRenderFileAsPdf ? "is-code" : ""} ${activeTab.kind === "diffs" ? "is-diff" : ""} ${activeTab.kind === "agent" ? "is-agent" : ""}`}
      >
        {activeTab.loading ? (
          <div className="summaryPanelHint">{t("summaryPanel.loadingFile")}</div>
        ) : null}
        {activeTab.error ? (
          <div className="summaryPanelHint is-error">{activeTab.error}</div>
        ) : null}
        {activeTab.kind === "diffs" ? (
          activeTab.diffPanel ? (
            <DiffPanelPreview
              data={activeTab.diffPanel}
              onOpenWorkspacePath={onOpenWorkspacePath}
            />
          ) : (
            <div className="summaryPanelHint is-error">
              {t("summaryPanel.diffPanelDataIsMissing")}
            </div>
          )
        ) : null}
        {activeTab.kind === "file" && !activeTab.loading && !activeTab.error ? (
          shouldRenderFileAsImage ? (
            <ImagePreview tab={activeTab} />
          ) : shouldRenderFileAsPdf ? (
            <PdfPreview tab={activeTab} />
          ) : !shouldRenderFileAsCode ? (
            <MarkdownPreview text={activeTab.content ?? ""} />
          ) : (
            <Suspense
              fallback={<div className="summaryPanelHint">{t("summaryPanel.loadingEditor")}</div>}
            >
              <CodePreview
                content={activeTab.content ?? ""}
                path={activeTab.path}
                targetLine={activeTab.targetLine}
                targetEndLine={activeTab.targetEndLine}
              />
            </Suspense>
          )
        ) : null}
        {activeTab.kind === "agent" && activeTab.sessionId ? (
          <AgentSessionPreview
            sessionId={activeTab.sessionId}
            onOpenWorkspacePath={onOpenWorkspacePath}
          />
        ) : null}
        {activeTab.kind === "files" ? (
          <div className="summaryFilePlaceholder">
            <FolderOpen aria-hidden="true" />
            <strong>Open a file</strong>
            <span>Select a file from the workspace tree</span>
          </div>
        ) : null}
        {activeTab.kind === "tasks" && activeTab.sessionId ? <WorkspaceTasks active={active} key={activeTab.sessionId} sessionId={activeTab.sessionId} /> : null}
        {activeTab.kind === "review" && activeTab.workspaceRoot ? (
          <WorkspaceReview root={activeTab.workspaceRoot} onOpenFile={onOpenWorkspacePath} />
        ) : null}
      </div>
    </>
  );
}

export default SummaryPanel;
