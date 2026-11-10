import { changeLanguage, useTranslation, type Language } from "../i18n";
import { ChevronDown } from "lucide-react";

export function LanguageSelector() {
  const { t, i18n } = useTranslation();
  return <label className="languageSelector">
    <span>{t("language.label")}</span>
    <span className="languageSelectorControl">
      <select value={i18n.resolvedLanguage} onChange={(event) => void changeLanguage(event.target.value as Language)}>
        <option value="en" lang="en">English</option>
        <option value="zh-CN" lang="zh-CN">简体中文</option>
      </select>
      <ChevronDown aria-hidden="true" />
    </span>
  </label>;
}
