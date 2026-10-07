import { languages } from "@codemirror/language-data";

const SPECIAL_LOADERS = Object.freeze({
  graphql: () => import("cm6-graphql").then(({ graphqlLanguageSupport }) => graphqlLanguageSupport()),
  makefile: () => import("./makefileLanguage.mjs").then(({ makefile }) => makefile()),
});

const LANGUAGE_DESCRIPTIONS = new Map();
for (const description of languages) {
  LANGUAGE_DESCRIPTIONS.set(description.name.toLowerCase(), description);
  for (const alias of description.alias) LANGUAGE_DESCRIPTIONS.set(alias.toLowerCase(), description);
}

export function codePreviewLanguageCanLoad(language) {
  return Object.hasOwn(SPECIAL_LOADERS, language) || LANGUAGE_DESCRIPTIONS.has(language);
}

export async function loadCodePreviewLanguage(language) {
  const specialLoader = SPECIAL_LOADERS[language];
  if (specialLoader) return specialLoader();
  const description = LANGUAGE_DESCRIPTIONS.get(language);
  if (!description) throw new Error(`unsupported code preview language: ${language}`);
  return description.load();
}
