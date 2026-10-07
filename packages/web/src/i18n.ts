import { createInstance } from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import zhCN from "./locales/zh-CN.json";
export { useTranslation } from "react-i18next";

export const LANGUAGE_STORAGE_KEY = "centaeris:language:v1";
export type Language = "en" | "zh-CN";
export function savedLanguage(): Language {
  try {
    return localStorage.getItem(LANGUAGE_STORAGE_KEY) === "en" ? "en" : "zh-CN";
  } catch {
    return "zh-CN";
  }
}

export const i18n = createInstance();
void i18n.use(initReactI18next).init({
  resources: { en: { translation: en }, "zh-CN": { translation: zhCN } },
  lng: savedLanguage(),
  fallbackLng: "en",
  supportedLngs: ["en", "zh-CN"],
  keySeparator: false,
  initAsync: false,
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});

function updateDocumentLanguage(language: string) {
  if (typeof document !== "undefined") document.documentElement.lang = language;
}
updateDocumentLanguage(i18n.language);
i18n.on("languageChanged", updateDocumentLanguage);

export async function changeLanguage(language: Language) {
  await i18n.changeLanguage(language);
  try { localStorage.setItem(LANGUAGE_STORAGE_KEY, language); } catch {
    // Keep in-memory language switching available when storage is blocked.
  }
}

// Non-React presentation helpers resolve copy at call time.
export const t: typeof i18n.t = i18n.t.bind(i18n);
