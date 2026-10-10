import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en.json";

/**
 * i18next setup. Resources are bundled, so initialization is synchronous and every `t()` call works on
 * first render. Importing this module is what initializes i18n; main.tsx and the test helper do so.
 */
export const resources = {
  en: { translation: en },
} as const;

if (!i18n.isInitialized) {
  void i18n.use(initReactI18next).init({
    lng: "en",
    fallbackLng: "en",
    resources,
    interpolation: { escapeValue: false },
    returnNull: false,
  });
}

export default i18n;
