import { useEffect, useRef, type ReactNode } from "react";

// Shared accessible frame for the overview and the existing Session/file stack.
// Switching between them must not restore focus outside the still-open panel.
export function AgentPreviewPanel({ label, focusKey, onClose, children }: Readonly<{
  label: string; focusKey: string; onClose: () => void; children: ReactNode;
}>) {
  const panelRef = useRef<HTMLElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const focusKeyRef = useRef(focusKey);
  useEffect(() => {
    const panel = panelRef.current;
    return () => {
      const opener = openerRef.current;
      requestAnimationFrame(() => {
        if (!panel?.isConnected && !document.querySelector(".agentChatSessionPreview") && opener?.isConnected && document.activeElement === document.body) opener.focus({ preventScroll: true });
      });
    };
  }, []);
  useEffect(() => {
    focusKeyRef.current = focusKey;
    const panel = panelRef.current;
    const active = document.activeElement;
    if (panel && active instanceof HTMLElement && active !== document.body && !panel.contains(active)) openerRef.current = active;
    const frame = requestAnimationFrame(() => {
      if (focusKeyRef.current === focusKey && panel && !panel.contains(document.activeElement)) (panel.querySelector<HTMLElement>("button") ?? panel).focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(frame);
  }, [focusKey]);
  return <aside className="agentChatSessionPreview" ref={panelRef} tabIndex={-1}
    role="complementary" aria-label={label}
    onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onClose(); } }}>
    {children}
  </aside>;
}
