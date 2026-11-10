import type { ReactNode } from "react";
import { Check, Monitor, Moon, Palette, Server, Sun } from "lucide-react";
import { changeTheme, useThemePreference, type ThemePreference } from "../theme";
import { Button } from "./ui/button";
import "../styles/desktop-settings.css";

export type SettingsSection = "appearance" | "models";
const themes: { value: ThemePreference; label: string; icon: typeof Monitor }[] = [
  { value: "system", label: "System", icon: Monitor },
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
];

export function SettingsNavigation({ section, onSelect, collapsed }: {
  section: SettingsSection;
  onSelect: (section: SettingsSection) => void;
  collapsed: boolean;
}) {
  return <aside className="thinSidebar desktopWorkbenchSidebar" aria-label="Settings" hidden={collapsed} inert={collapsed}>
    <div className="desktopSidebarBrand"><strong>Settings</strong></div>
    <nav className="appNavigation" aria-label="Settings categories">
      <Button variant="ghost" aria-current={section === "appearance" ? "page" : undefined} onClick={() => onSelect("appearance")}><Palette size={16} />Appearance</Button>
      <Button variant="ghost" aria-current={section === "models" ? "page" : undefined} onClick={() => onSelect("models")}><Server size={16} />Model services</Button>
    </nav>
  </aside>;
}

export function SettingsPage({ children, section }: {
  children: ReactNode;
  section: SettingsSection;
}) {
  const theme = useThemePreference();
  return <section className="desktopSettings" aria-label="Settings content">
    <div className="desktopSettingsContent">{section === "models" ? children : <section>
      <header className="desktopSettingsHeading"><h1>Appearance</h1></header>
      <section className="desktopSettingsSection" aria-labelledby="themeHeading">
        <div><h2 id="themeHeading">Theme</h2></div>
        <div className="desktopThemeChoices" role="group" aria-label="Theme">
          {themes.map(({ value, label, icon: Icon }) => <Button key={value} variant="outline" className="desktopThemeChoice" aria-label={label} aria-pressed={theme === value} onClick={() => changeTheme(value)}>
            <span className={"desktopThemePreview desktopThemePreview--" + value} aria-hidden="true"><span className="desktopThemePreviewSidebar"><i /><i /><i /></span><span className="desktopThemePreviewContent"><i /><i /><i /></span></span>
            <span className="desktopThemeLabel"><Icon size={15} /><strong>{label}</strong>{theme === value ? <Check size={15} /> : null}</span>
          </Button>)}
        </div>
      </section>
    </section>}</div>
  </section>;
}
