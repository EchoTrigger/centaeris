import { useEffect } from "react";
import { useThemePreference } from "../theme";
import { isNativeHostRuntime } from "../host/hostBridge";
import { ArrowLeft, ArrowRight, PanelLeft } from "lucide-react";
import { invokeHost } from "../host/hostBridge";
export function ApplicationBar({ expanded, onToggle, onBack, onForward, canBack, canForward, onError }: {
  expanded: boolean; onToggle: () => void; onBack: () => void; onForward: () => void; canBack: boolean; canForward: boolean; onError: (error: string) => void;
}) {
  const preference = useThemePreference();
  useEffect(() => { if (isNativeHostRuntime()) void invokeHost("desktop_theme", { preference }).catch(error => onError(String(error))); }, [preference, onError]);
  return <header className="nativeTitlebar"><div className="nativeTitlebarSafeArea">
    <button className="nativePanelToggle" aria-label="Back" disabled={!canBack} onClick={onBack}><ArrowLeft aria-hidden="true" /></button>
    <button className="nativePanelToggle" aria-label="Forward" disabled={!canForward} onClick={onForward}><ArrowRight aria-hidden="true" /></button>
    <button className="nativePanelToggle" aria-label={expanded ? "Hide left sidebar" : "Show left sidebar"} aria-expanded={expanded} onClick={onToggle}><PanelLeft aria-hidden="true" /></button>
    <nav className="applicationMenus" aria-label="Application menu">{["File", "Edit", "View", "Help"].map(menu => <button type="button" key={menu} aria-haspopup="menu" onClick={event => { const bounds = event.currentTarget.getBoundingClientRect(); void invokeHost("desktop_menu", { menu, anchor: { x: bounds.left, y: bounds.bottom } }).catch(error => onError(String(error))); }}>{menu}</button>)}</nav>
  </div></header>;
}
