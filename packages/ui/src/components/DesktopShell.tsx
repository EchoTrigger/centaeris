import { desktopSidebarTransition } from "../lib/desktopMotion";
import { useAnimate } from "motion/react";
import { useLayoutEffect, useRef, type CSSProperties, type ReactNode } from "react";

type SidebarStyle = CSSProperties & { "--sidebar-progress": number };
export function DesktopShell({ expanded, className, children }: { expanded: boolean; className?: string; children: ReactNode }) {
  const [scope, animate] = useAnimate<HTMLDivElement>();
  const previous = useRef(expanded);
  const initialStyle = useRef<SidebarStyle>({
    "--sidebar-progress": expanded ? 1 : 0,
    // The responsive CSS width remains authoritative even during an animation.
    gridTemplateColumns: "calc(var(--thin-sidebar-width) * var(--sidebar-progress)) minmax(0, 1fr)",
  });
  useLayoutEffect(() => {
    const changed = previous.current !== expanded;
    previous.current = expanded;
    const element = scope.current;
    if (!element || !changed) return;
    const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
    const target = expanded ? 1 : 0;
    const playback = animate(element, { "--sidebar-progress": target },
      { ...desktopSidebarTransition, duration: preference.matches ? 0 : desktopSidebarTransition.duration, reduceMotion: false });
    const reduce = () => {
      if (!preference.matches) return;
      playback.complete();
      element.style.setProperty("--sidebar-progress", String(target));
    };
    reduce();
    preference.addEventListener("change", reduce);
    void playback.then(() => preference.removeEventListener("change", reduce));
    return () => {
      preference.removeEventListener("change", reduce);
      playback.stop();
    };
  }, [expanded, animate, scope]);
  return <div ref={scope} className={className} style={initialStyle.current}>{children}</div>;
}
