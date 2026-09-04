import {
  lazy,
  Suspense,
  useMemo,
  useState,
} from "react";
import {
  Ellipsis,
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
import { AgentSessionPreview } from "./chat/AgentSessionPreview";

export type SummaryPanelTab = {
  id: string;
  title: string;
  kind: "summary" | "file" | "diffs" | "agent";
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

const CodePreview = lazy(() => import("./CodePreview"));

type OpenWorkspacePathOptions = {
  taskId?: string;
};

type SummaryPanelProps = {
  tabs: SummaryPanelTab[];
  activeTabId: string | null;
  onSelectTab: (tabId: string) => void;
  onCloseTab: (tabId: string) => void;
  onAddSummaryTab?: () => void;
  onCollapse?: () => void;
  showTabStrip?: boolean;
  onOpenWorkspacePath?: (
    path: string,
    options?: OpenWorkspacePathOptions,
  ) => void;
};

const getActiveTab = (tabs: SummaryPanelTab[], activeTabId: string | null): SummaryPanelTab | null => {
  if (!activeTabId) {
    return tabs[0] ?? null;
  }
  return tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null;
};

const isMarkdownPath = (path: string | undefined): boolean => {
  const normalizedPath = (path || "").toLowerCase();
  return normalizedPath.endsWith(".md") || normalizedPath.endsWith(".markdown") || normalizedPath.endsWith(".mdx");
};

function MarkdownPreview({ text }: { text: string }) {
  if (!text.trim()) {
    return <div className="summaryPanelHint">文件为空。</div>;
  }
  return <div className="summaryMarkdownContent">{renderMarkdownNodes(text)}</div>;
}

function ImagePreview({ tab }: { tab: SummaryPanelTab }) {
  if (!tab.dataUrl) {
    return <div className="summaryPanelHint is-error">图片预览缺少 data URL。</div>;
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
    return <div className="summaryPanelHint is-error">PDF 预览缺少 data URL。</div>;
  }
  return (
    <div className="summaryPdfPreview">
      <iframe
        title={tab.title}
        src={tab.dataUrl}
        className="summaryPdfFrame"
      />
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
      <span className="summaryDiffStat is-added">+{normalizeDiffCount(added).toLocaleString()}</span>
      <span className="summaryDiffStat is-removed">-{normalizeDiffCount(removed).toLocaleString()}</span>
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
  onOpenWorkspacePath?: (
    path: string,
    options?: OpenWorkspacePathOptions,
  ) => void;
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
            {file.diffUnavailableReason || "不可审查"}
          </span>
        ) : (
          <DiffStats added={file.added} removed={file.removed} compact />
        )}
        {onOpenWorkspacePath ? (
          <Tooltip align="end" content="打开文件">
            <Button
              type="button"
              variant="workspace"
              size="workspaceIcon"
              className="summaryDiffOpenButton"
              aria-label={`打开 ${file.path}`}
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
  onOpenWorkspacePath?: (
    path: string,
    options?: OpenWorkspacePathOptions,
  ) => void;
}) {
  const [selectedFileId, setSelectedFileId] = useState<string | null>(
    data.files[0]?.id ?? null,
  );
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
    return <div className="summaryPanelHint is-error">diff 面板没有可显示的文件。</div>;
  }

  return (
    <div className="summaryDiffPanel">
      <div className="summaryDiffPanelHeader">
        <div className="summaryDiffPanelHeading">
          <strong>审查</strong>
          <span>{data.files.length.toLocaleString()} 个文件</span>
        </div>
        <DiffStats added={totals.added} removed={totals.removed} />
      </div>
      <div className="summaryDiffPanelContent">
        <div className="summaryDiffPreview">
          {selectedFile?.diffAvailable === false ? (
            <div className="summaryPanelHint">
              {selectedFile.diffUnavailableReason || "该文件暂不支持 diff 审查。"}
            </div>
          ) : selectedFile ? (
            <Suspense fallback={<div className="summaryPanelHint">正在加载 diff...</div>}>
              <CodePreview
                content={selectedFile.diffPreview}
                path={selectedFile.path}
                variant="diff"
              />
            </Suspense>
          ) : null}
        </div>
        <div className="summaryDiffFileList" aria-label="审查文件列表">
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
  const agentTabs = tabs.filter((tab) => tab.kind === "agent");
  const shouldRenderFileAsImage = activeTab?.kind === "file" && activeTab.contentKind === "image";
  const shouldRenderFileAsPdf = activeTab?.kind === "file" && activeTab.contentKind === "pdf";
  const shouldRenderFileAsCode = activeTab?.kind === "file"
    && !shouldRenderFileAsImage
    && !shouldRenderFileAsPdf
    && (!isMarkdownPath(activeTab.path) || typeof activeTab.targetLine === "number");

  return (
    <section className="summaryPanel" aria-label="右侧面板">
      {showTabStrip ? (
        <div className="summaryPanelTabStrip">
          <div className="summaryPanelTabs">
            {tabs.map((tab) => (
              <button
                type="button"
                className={`summaryPanelTab ${activeTab?.id === tab.id ? "is-active" : ""}`}
                aria-label={`打开 ${tab.path ?? tab.title}`}
                key={tab.id}
                onClick={() => onSelectTab(tab.id)}
              >
                <span className="summaryPanelTabTitle">{tab.title}</span>
                <span
                  role="button"
                  tabIndex={0}
                  className="summaryPanelTabClose"
                  aria-label={`关闭 ${tab.title}`}
                  onClick={(event) => {
                    event.stopPropagation();
                    onCloseTab(tab.id);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      event.stopPropagation();
                      onCloseTab(tab.id);
                    }
                  }}
                >
                  <X className="summaryPanelIcon is-close" aria-hidden="true" />
                </span>
              </button>
            ))}
            {onAddSummaryTab ? (
              <Tooltip content="打开概括">
                <Button
                  type="button"
                  variant="workspace"
                  size="chromeIcon"
                  className="summaryPanelTabAdd"
                  onClick={onAddSummaryTab}
                  aria-label="打开概括"
                >
                  <Plus className="summaryPanelIcon" aria-hidden="true" />
                </Button>
              </Tooltip>
            ) : null}
          </div>
          {onCollapse ? (
            <Tooltip align="end" content="收起右侧面板">
              <button
                type="button"
                className="summaryPanelCollapse"
                onClick={onCollapse}
                aria-label="收起右侧面板"
              >
                <PanelRight className="summaryPanelIcon" aria-hidden="true" />
              </button>
            </Tooltip>
          ) : null}
        </div>
      ) : null}
      {activeTab ? (
        <>
          <header className="summaryPanelHeader">
            <div className="summaryPanelTitleGroup">
              {activeTab.kind === "agent" ? (
                <h1 className="agentSessionPanelBreadcrumb">
                  <span>{activeTab.parentTitle || "主会话"}</span>
                  <span aria-hidden="true">/</span>
                  <strong>{activeTab.title}</strong>
                </h1>
              ) : <h1>{activeTab.title}</h1>}
              {activeTab.kind === "file" && activeTab.path ? <span>{activeTab.path}</span> : null}
              {activeTab.kind === "diffs" && activeTab.diffPanel ? <span>{activeTab.diffPanel.subtitle}</span> : null}
            </div>
            <Tooltip align="end" content="更多">
              <Button
                type="button"
                variant="workspace"
                size="workspaceIcon"
                className="summaryPanelMore"
                aria-label="更多"
              >
                <Ellipsis className="summaryPanelIcon" aria-hidden="true" />
              </Button>
            </Tooltip>
          </header>
          <div
            className={`summaryPanelBody ${shouldRenderFileAsCode || shouldRenderFileAsPdf ? "is-code" : ""} ${activeTab.kind === "diffs" ? "is-diff" : ""} ${activeTab.kind === "agent" ? "is-agent" : ""}`}
          >
            {activeTab.loading ? <div className="summaryPanelHint">正在读取文件...</div> : null}
            {activeTab.error ? <div className="summaryPanelHint is-error">{activeTab.error}</div> : null}
            {activeTab.kind === "diffs" ? (
              activeTab.diffPanel ? (
                <DiffPanelPreview
                  data={activeTab.diffPanel}
                  onOpenWorkspacePath={onOpenWorkspacePath}
                />
              ) : (
                <div className="summaryPanelHint is-error">diff 面板数据缺失。</div>
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
                <Suspense fallback={<div className="summaryPanelHint">正在加载编辑器...</div>}>
                  <CodePreview content={activeTab.content ?? ""} path={activeTab.path} targetLine={activeTab.targetLine} targetEndLine={activeTab.targetEndLine} />
                </Suspense>
              )
            ) : null}
            {agentTabs.map((tab) => tab.sessionId ? (
              <div
                className="summaryAgentSession"
                hidden={activeTab.id !== tab.id}
                key={tab.id}
              >
                <AgentSessionPreview
                  sessionId={tab.sessionId}
                  onOpenWorkspacePath={onOpenWorkspacePath}
                />
              </div>
            ) : null)}
          </div>
        </>
      ) : null}
    </section>
  );
}

export default SummaryPanel;
