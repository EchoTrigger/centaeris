import { useEffect, useState, type ReactNode } from "react";
import { Check, Monitor, Moon, Palette, Server, Sun } from "lucide-react";
import { changeTheme, useThemePreference, type ThemePreference } from "../theme";
import { Button } from "./ui/button";
import "../styles/desktop-settings.css";

type SettingsSection = "appearance" | "models";
const themes: { value: ThemePreference; label: string; description: string; icon: typeof Monitor }[] = [
  { value: "system", label: "System", description: "Follow your device", icon: Monitor },
  { value: "light", label: "Light", description: "A bright workspace", icon: Sun },
  { value: "dark", label: "Dark", description: "A quieter workspace", icon: Moon },
];

export function SettingsPage({ children, initialSection = "appearance" }: {
  children: ReactNode;
  initialSection?: SettingsSection;
}) {
  const [section, setSection] = useState<SettingsSection>(initialSection);
  const theme = useThemePreference();
  useEffect(() => setSection(initialSection), [initialSection]);
  return <section className="desktopSettings">
    <nav className="desktopSettingsNav" aria-label="Settings categories">
      <p>Preferences</p>
      <Button variant="ghost" aria-current={section === "appearance" ? "page" : undefined} onClick={() => setSection("appearance")}><Palette size={16} />Appearance</Button>
      <Button variant="ghost" aria-current={section === "models" ? "page" : undefined} onClick={() => setSection("models")}><Server size={16} />Model services</Button>
    </nav>
    <div className="desktopSettingsContent">{section === "models" ? children : <section>
      <header className="desktopSettingsHeading"><h1>Appearance</h1><p>Make Centaeris feel at home on your device.</p></header>
      <section className="desktopSettingsSection" aria-labelledby="themeHeading">
        <div><h2 id="themeHeading">Theme</h2><p>Choose a color scheme for the desktop app.</p></div>
        <div className="desktopThemeChoices" role="group" aria-label="Theme">
          {themes.map(({ value, label, description, icon: Icon }) => <Button key={value} variant="outline" className="desktopThemeChoice" aria-label={label} aria-pressed={theme === value} onClick={() => changeTheme(value)}>
            <span className={`desktopThemePreview desktopThemePreview--${value}`} aria-hidden="true"><span className="desktopThemePreviewSidebar"><i /><i /><i /></span><span className="desktopThemePreviewContent"><i /><i /><i /></span></span>
            <span className="desktopThemeLabel"><Icon size={15} /><strong>{label}</strong>{theme === value ? <Check size={15} /> : null}</span>
            <span className="desktopThemeDescription">{description}</span>
          </Button>)}
        </div>
        <p className="desktopSettingsNote">{theme === "system" ? "Centaeris automatically switches between light and dark when your device does." : `The ${theme} theme stays selected until you change it. Choose System to follow your device.`}</p>
      </section>
    </section>}</div>
  </section>;
}
