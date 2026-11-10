import { ChevronDown } from "lucide-react";
import { useTranslation } from "../i18n";
import { changeTheme, useThemePreference, type ThemePreference } from "../theme";

export function ThemeSelector() {
  const { t } = useTranslation();
  const preference = useThemePreference();
  return <label className="themeSelector">
    <span>{t("theme.label")}</span>
    <span className="themeSelectorControl">
      <select value={preference} onChange={(event) => changeTheme(event.target.value as ThemePreference)}>
        <option value="system">{t("theme.system")}</option>
        <option value="dark">{t("theme.dark")}</option>
        <option value="light">{t("theme.light")}</option>
      </select>
      <ChevronDown aria-hidden="true" />
    </span>
  </label>;
}
