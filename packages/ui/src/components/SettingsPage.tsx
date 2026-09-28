import { useState, type ReactNode } from "react";
import { changeTheme, useThemePreference, type ThemePreference } from "../theme";
export function SettingsPage({ children }: { children: ReactNode }) {
  const [section, setSection] = useState("appearance");
  const theme = useThemePreference();
  return <section className="settingsPage">
    <nav aria-label="Settings categories">{[["appearance", "Appearance"], ["models", "Model services"]].map(([id, name]) => <button type="button" key={id} aria-current={section === id ? "page" : undefined} onClick={() => setSection(id)}>{name}</button>)}</nav>
    <div className="settingsContent">{section === "models" ? children : <section className="appearanceSettings"><h1>Appearance</h1><div className="settingsRow"><label htmlFor="themePreference">Theme</label><select id="themePreference" value={theme} onChange={event => changeTheme(event.target.value as ThemePreference)}><option value="system">System</option><option value="light">Light</option><option value="dark">Dark</option></select></div></section>}</div>
  </section>;
}
