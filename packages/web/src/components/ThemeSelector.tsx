import { Monitor, Moon, Sun } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "../i18n";
import { changeTheme, useThemePreference } from "../theme";

const themes = [
  { value: "system", icon: Monitor },
  { value: "light", icon: Sun },
  { value: "dark", icon: Moon },
] as const;

export function ThemeSelector() {
  const { t } = useTranslation();
  const preference = useThemePreference();
  const id = useId();
  return <section className="settingsPanel settingsThemePanel" aria-labelledby={id}>
    <h2 id={id}>{t("theme.label")}</h2>
    <div className="settingsThemeChoices" role="radiogroup" aria-labelledby={id}>
      {themes.map(({ value, icon: Icon }) => <label key={value}>
        <input className="srOnly" type="radio" name={id} value={value} checked={preference === value} onChange={() => changeTheme(value)} />
        <span className="settingsThemeChoice">
          <span className={`settingsThemePreview is-${value}`} aria-hidden="true">
            <span className="settingsThemeRail"><i /><i /><i /></span>
            <span className="settingsThemeContent"><i /><i /><i /></span>
          </span>
          <span className="settingsThemeLabel">
            <Icon size={15} aria-hidden="true" />
            <span>{t(`theme.${value}`)}</span>
          </span>
        </span>
      </label>)}
    </div>
  </section>;
}
