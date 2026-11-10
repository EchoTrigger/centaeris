import { createInstance } from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";

export { useTranslation } from "react-i18next";
export const i18n = createInstance();
// Desktop ships English only. Add complete resources before exposing another locale.
void i18n.use(initReactI18next).init({
  resources: { en: { translation: en } },
  lng: "en",
  fallbackLng: "en",
  supportedLngs: ["en"],
  keySeparator: false,
  initAsync: false,
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});
if (typeof document !== "undefined") document.documentElement.lang = "en";
export const t: typeof i18n.t = i18n.t.bind(i18n);
