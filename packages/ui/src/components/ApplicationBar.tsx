import { useEffect } from "react";
import { useThemePreference } from "../theme";
import { isNativeHostRuntime } from "../host/hostBridge";
import { ArrowLeft, ArrowRight, PanelLeft } from "lucide-react";
import { invokeHost } from "../host/hostBridge";
import { Button } from "./ui/button";
import { ThemeToggle } from "./ThemeToggle";
export function ApplicationBar({ expanded, onToggle, onBack, onForward, canBack, canForward, onError }: {
  expanded: boolean; onToggle: () => void; onBack: () => void; onForward: () => void; canBack: boolean; canForward: boolean; onError: (error: string) => void;
}) {
  const preference = useThemePreference();
  useEffect(() => { if (isNativeHostRuntime()) void invokeHost("desktop_theme", { preference }).catch(error => onError(String(error))); }, [preference, onError]);
  return <header className="nativeTitlebar" aria-label="Window toolbar"><div className="nativeTitlebarSafeArea">
    <ThemeToggle />
    <span className="desktopChromeDivider" aria-hidden="true" />
    <Button variant="ghost" size="icon-sm" className="desktopChromeButton" aria-label="Back" disabled={!canBack} onClick={onBack}><ArrowLeft aria-hidden="true" /></Button>
    <Button variant="ghost" size="icon-sm" className="desktopChromeButton" aria-label="Forward" disabled={!canForward} onClick={onForward}><ArrowRight aria-hidden="true" /></Button>
    <Button variant="ghost" size="icon-sm" className="desktopChromeButton" aria-label={expanded ? "Hide left sidebar" : "Show left sidebar"} aria-expanded={expanded} onClick={onToggle}><PanelLeft aria-hidden="true" /></Button>
    <nav className="applicationMenus" aria-label="Application menu">{["File", "Edit", "View", "Help"].map(menu => <Button variant="ghost" size="sm" type="button" key={menu} aria-haspopup="menu" onClick={event => { const bounds = event.currentTarget.getBoundingClientRect(); void invokeHost("desktop_menu", { menu, anchor: { x: bounds.left, y: bounds.bottom } }).catch(error => onError(String(error))); }}>{menu}</Button>)}</nav>
  </div></header>;
}
