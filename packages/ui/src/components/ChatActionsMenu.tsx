import { useEffect, useRef, type ReactNode } from "react";
import { MoreHorizontal } from "lucide-react";

export function ChatActionsMenu({ children, label = "Chat actions" }: { children: ReactNode; label?: string }) {
  const menu = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    if (typeof document === "undefined") return;
    const dismiss = (event: PointerEvent) => {
      if (menu.current && !menu.current.contains(event.target as Node)) menu.current.open = false;
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, []);
  return <details className="chatActionsMenu" ref={menu} onKeyDown={event => {
    if (event.key === "Escape" && menu.current) {
      menu.current.open = false;
      menu.current.querySelector("summary")?.focus();
    }
  }}>
    <summary aria-label={label}><MoreHorizontal size={18} /></summary>
    <div onClick={() => { if (menu.current) menu.current.open = false; }}>{children}</div>
  </details>;
}
