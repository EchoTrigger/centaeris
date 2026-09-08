import { ChevronDown, Moon, Sun } from "lucide-react";
import { useTranslation } from "../i18n";
import { changeThemeAnimated, useResolvedTheme, useThemePreference, type ThemePreference } from "../theme";
export function ThemeToggle() {
  const { t } = useTranslation();
  const resolved = useResolvedTheme();
  const preference = useThemePreference();
  const apply = (value: ThemePreference, element: HTMLElement) => {
    const rect = element.getBoundingClientRect();
    changeThemeAnimated(value, { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
  };
  return <div className="themeToggle">
    <button type="button" aria-label={t(resolved === "dark" ? "theme.switchLight" : "theme.switchDark")}
      title={t(resolved === "dark" ? "theme.switchLight" : "theme.switchDark")}
      onClick={event => apply(resolved === "dark" ? "light" : "dark", event.currentTarget)}>
      {resolved === "dark" ? <Moon aria-hidden="true" /> : <Sun aria-hidden="true" />}
    </button>
    <span className="themeToggleOptions"><select aria-label={t("theme.label")} value={preference}
      onChange={event => apply(event.target.value as ThemePreference, event.currentTarget)}>
      <option value="system">{t("theme.system")}</option><option value="dark">{t("theme.dark")}</option><option value="light">{t("theme.light")}</option>
    </select><ChevronDown aria-hidden="true" /></span>
  </div>;
}
