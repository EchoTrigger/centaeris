
import { useTranslation } from "../i18n";
import { Download, X } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import { apiUrl } from "../api";
import { DocumentPreview } from "./DocumentPreview";
import { TextFilePreview } from "./TextFilePreview";

function FilePreviewPanel({ panel, browserWidthPx, onBrowserWidthChange, onClose, onReturn }) {
  const { t } = useTranslation();
  const dragRef = useRef(null);
  const handleRef = useRef(null);
  const [geometry, setGeometry] = useState({ width: browserWidthPx ?? 270, min: 270, max: 270 });
  useLayoutEffect(() => {
    const panel = handleRef.current.parentElement;
    const workbench = panel.closest(".workspaceWorkbench");
    const region = workbench?.querySelector(".workspaceChatColumn") || workbench;
    const measure = () => {
      const stacked = workbench && (getComputedStyle(workbench).display !== "grid" || getComputedStyle(panel).gridColumnStart !== "3");
      const available = stacked ? region.getBoundingClientRect().width - 32 : Math.min(window.innerWidth * 0.75, region ? region.getBoundingClientRect().width + panel.getBoundingClientRect().width - 480 : window.innerWidth * 0.75);
      const max = Math.max(0, Math.floor(available));
      const min = Math.min(max, Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--workspace-sidebar-width")) || 270);
      const width = panel.getBoundingClientRect().width;
      setGeometry(previous => previous.width === width && previous.min === min && previous.max === max ? previous : { width, min, max });
    };
    const observer = new ResizeObserver(measure);
    observer.observe(panel);
    if (workbench) observer.observe(workbench);
    if (region && region !== workbench) observer.observe(region);
    measure();
    return () => observer.disconnect();
  }, []);
  const changeWidth = width => onBrowserWidthChange(Math.min(geometry.max, Math.max(geometry.min, width)));
  const moveResizeHandle = (clientX) => {
    const drag = dragRef.current;
    if (drag) changeWidth(drag.widthPx + drag.clientX - clientX);
  };
  return (
    <>
      <div
        ref={handleRef}
        className="workspaceContextPanelResizeHandle"
        role="separator"
        aria-label={t("workspaceContextPanel.resizePreviewPanel")}
        aria-orientation="vertical"
        aria-valuemin={geometry.min}
        aria-valuemax={geometry.max}
        aria-valuenow={geometry.width}
        tabIndex={0}
        onKeyDown={(event) => {
          if (event.key === "ArrowLeft") changeWidth(geometry.width + 16);
          else if (event.key === "ArrowRight") changeWidth(geometry.width - 16);
          else return;
          event.preventDefault();
        }}
        onPointerDown={(event) => {
          dragRef.current = { clientX: event.clientX, widthPx: geometry.width };
          event.currentTarget.setPointerCapture(event.pointerId);
        }}
        onPointerMove={(event) => moveResizeHandle(event.clientX)}
        onPointerUp={(event) => {
          moveResizeHandle(event.clientX);
          dragRef.current = null;
          event.currentTarget.releasePointerCapture(event.pointerId);
        }}
        onPointerCancel={() => { dragRef.current = null; }}
      />
      <header className="workspaceContextPanelHeader filePreviewHeader">
        <nav aria-label={t("workspaceContextPanel.filePreviewPath")}>
          <button type="button" onClick={onReturn}>
            {panel.citationId ? t("workspaceContextPanel.library") : panel.originLabel || t("workspaceContextPanel.library")}
          </button>
          <span aria-hidden="true">/</span>
          <strong title={panel.displayName}>{panel.displayName}</strong>
        </nav>
        <div className="filePreviewActions">
          {panel.downloadUrl ? (
            <a href={apiUrl(panel.downloadUrl)} aria-label={t("workspaceContextPanel.downloadValue", { value1: panel.displayName })} title={t("workspaceContextPanel.download")}>
              <Download aria-hidden="true" />
            </a>
          ) : null}
          <button className="workspaceContextPanelClose" type="button" onClick={onClose} aria-label={t("attachmentCard.closePreview")} title={t("workspaceContextPanel.close")}>
            <X aria-hidden="true" />
          </button>
        </div>
      </header>
      {panel.status === "loading" ? <div className="filePreviewState">{t("workspaceContextPanel.loadingReferenceFile")}</div> : null}
      {panel.status === "error" ? <div className="filePreviewState isError" role="alert">{panel.error}</div> : null}
      {panel.status === "ready" ? (
        <div className="filePreviewLayout">
          <div className="filePreviewBody">
            {panel.preview.kind === "text" ? <TextFilePreview content={panel.preview.content} contentType={panel.preview.contentType} title={panel.displayName} locator={panel.locator} renderMarkdown={!panel.locator} /> : null}
            {panel.preview.kind === "pdf" ? <iframe src={panel.preview.src} title={panel.displayName} /> : null}
            {panel.preview.kind === "office" ? <DocumentPreview src={panel.preview.src} title={panel.displayName} /> : null}
            {panel.preview.kind === "image" ? <img src={panel.preview.src} alt={panel.displayName} /> : null}
            {panel.preview.kind === "unsupported" ? (
              <div className="filePreviewState">{t("workspaceContextPanel.inlinePreviewIsUnavailableForThisFileTypeUse")}</div>
            ) : null}
          </div>
        </div>
      ) : null}
    </>
  );
}

export function WorkspaceContextPanel({ panel, browserWidthPx, onBrowserWidthChange, onClose, onReturn }) {
  const { t } = useTranslation();
  const open = panel.mode === "filePreview";
  // Keep only the empty shell for the exit transition. Closing immediately
  // releases file content and its resources; no stale citation is retained.
  return <aside className="workspaceContextPanel workspaceFilePreviewPanel" aria-label={t("workspaceContextPanel.filePreview")}
    inert={!open} aria-hidden={!open}>
    {open ? <FilePreviewPanel panel={panel} browserWidthPx={browserWidthPx} onBrowserWidthChange={onBrowserWidthChange} onClose={onClose} onReturn={onReturn} /> : null}
  </aside>;
}
