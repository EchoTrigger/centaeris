import { animate } from "@centaeris/frontend-foundations/motion-react";
import { desktopSidebarTransition } from "../lib/desktopMotion";
import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Group, Panel, useGroupRef, type Layout } from "@centaeris/frontend-foundations/panels";
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
  const previous = useRef({ visible: previewVisible, compact: false, placed: false });
  const [moving, setMoving] = useState(false);
  const [settledVisible, setSettledVisible] = useState(previewVisible);
  const transitioning = moving || settledVisible !== previewVisible;
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
    const before = previous.current;
    previous.current = { visible: previewVisible, compact, placed: true };
    const target = !previewVisible ? { chat: 100, preview: 0 }
      : compact ? { chat: 0, preview: 100 } : rememberedLayout.current;
    const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
    if (!before.placed || before.visible === previewVisible || before.compact || compact || hidden || preference.matches) {
      setMoving(false); setSettledVisible(previewVisible); groupRef.current?.setLayout(target); return;
    }
    // Motion owns pacing; the panel library owns every intermediate layout.
    const from = groupRef.current?.getLayout().preview ?? (before.visible ? rememberedLayout.current.preview : 0);
    let owned = true;
    setMoving(true);
    const playback = animate(from, target.preview, {
      ...desktopSidebarTransition,
      onUpdate: preview => { if (owned) groupRef.current?.setLayout({ chat: 100 - preview, preview }); },
    });
    const finish = () => {
      if (!owned) return;
      groupRef.current?.setLayout(target); setMoving(false); setSettledVisible(previewVisible);
    };
    const reduce = () => { if (preference.matches) { playback.complete(); finish(); } };
    preference.addEventListener("change", reduce);
    void playback.then(() => { preference.removeEventListener("change", reduce); finish(); });
    return () => { owned = false; preference.removeEventListener("change", reduce); playback.stop(); };
  }, [availableWidth, compact, previewVisible, hidden, groupRef]);
  const reset = () => {
    rememberedLayout.current = equalSplit;
    groupRef.current?.setLayout(equalSplit);
  };
  return <div ref={container} className="thinWorkspaceBody desktopWorkspaceLayout" hidden={hidden}
    data-preview-animating={transitioning || undefined} data-compact={compact || undefined} data-preview-visible={previewVisible || undefined}>
    <Group id="desktop-workspace-layout" groupRef={groupRef} defaultLayout={equalSplit}
      className="desktopWorkspaceGroup" orientation="horizontal" disabled={compact || !previewVisible || transitioning}
      onLayoutChanged={(layout, meta) => {
        if (meta.isUserInteraction && previewVisible && !compact) rememberedLayout.current = meta.requestedLayout ?? layout;
      }}>
      <Panel id="chat" defaultSize="50%" minSize={compact || !previewVisible || transitioning ? 0 : 320}
        maxSize={compact || !previewVisible || transitioning ? "100%" : "75%"}
        collapsible={compact || transitioning} className="desktopChatPanel" aria-hidden={compact && previewVisible}
        inert={compact && previewVisible}>
        {children}
      </Panel>
      <WorkspaceSplitHandle disabled={compact || !previewVisible || transitioning} onReset={reset} />
      <Panel id="preview" defaultSize="50%" minSize={compact || !previewVisible || transitioning ? 0 : 320}
        maxSize={compact || !previewVisible || transitioning ? "100%" : "75%"} collapsible={!previewVisible || transitioning}
        className="desktopPreviewPanel" aria-hidden={!previewVisible} inert={!previewVisible}>
        {preview}
      </Panel>
    </Group>
  </div>;
}
