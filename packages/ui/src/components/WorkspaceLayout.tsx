import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Group, Panel, useGroupRef, type Layout } from "react-resizable-panels";
import { WorkspaceSplitHandle } from "./WorkspaceSplitHandle";
import "../styles/desktop-layout.css";

const equalSplit: Layout = { chat: 50, preview: 50 };

export function WorkspaceLayout({ children, preview, previewVisible, hidden = false }: {
  children: ReactNode;
  preview: ReactNode;
  previewVisible: boolean;
  hidden?: boolean;
}) {
  const container = useRef<HTMLDivElement>(null);
  const groupRef = useGroupRef();
  const rememberedLayout = useRef(equalSplit);
  const [availableWidth, setAvailableWidth] = useState(0);
  const compact = availableWidth > 0 && availableWidth <= 800;
  useLayoutEffect(() => {
    const element = container.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const update = () => {
      const width = element.getBoundingClientRect().width;
      // Preserve the desktop split while the page is hidden or has no layout yet.
      if (width > 0) setAvailableWidth(width);
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    if (!container.current || availableWidth <= 0) return;
    // Reapply the requested split when the container grows after a temporary
    // pixel constraint. setLayout may otherwise remember the clamped percentage.
    groupRef.current?.setLayout(!previewVisible ? { chat: 100, preview: 0 }
      : compact ? { chat: 0, preview: 100 } : rememberedLayout.current);
  }, [availableWidth, compact, previewVisible, groupRef]);
  const reset = () => {
    rememberedLayout.current = equalSplit;
    groupRef.current?.setLayout(equalSplit);
  };
  return <div ref={container} className="thinWorkspaceBody desktopWorkspaceLayout" hidden={hidden}
    data-compact={compact || undefined} data-preview-visible={previewVisible || undefined}>
    <Group id="desktop-workspace-layout" groupRef={groupRef} defaultLayout={equalSplit}
      className="desktopWorkspaceGroup" orientation="horizontal" disabled={compact || !previewVisible}
      onLayoutChanged={(layout, meta) => {
        if (meta.isUserInteraction && previewVisible && !compact) rememberedLayout.current = meta.requestedLayout ?? layout;
      }}>
      <Panel id="chat" defaultSize="50%" minSize={compact || !previewVisible ? 0 : 320}
        maxSize={compact || !previewVisible ? "100%" : "75%"}
        collapsible={compact} className="desktopChatPanel" aria-hidden={compact && previewVisible}
        inert={compact && previewVisible}>
        {children}
      </Panel>
      <WorkspaceSplitHandle disabled={compact || !previewVisible} onReset={reset} />
      <Panel id="preview" defaultSize="50%" minSize={compact || !previewVisible ? 0 : 320}
        maxSize={compact || !previewVisible ? "100%" : "75%"} collapsible={!previewVisible}
        className="desktopPreviewPanel" aria-hidden={!previewVisible} inert={!previewVisible}>
        {preview}
      </Panel>
    </Group>
  </div>;
}
