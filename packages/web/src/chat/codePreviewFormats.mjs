import catalog from "../../../code_preview_languages.json" with { type: "json" };

if (catalog.schema !== "centaeris.code_preview_languages.v1") {
  throw new Error("unsupported code preview language catalog");
}

const EXTENSION_LANGUAGES = new Map();
const EXACT_NAME_LANGUAGES = new Map();
const CONTENT_TYPE_LANGUAGES = new Map();

for (const language of catalog.languages) {
  for (const extension of language.extensions) EXTENSION_LANGUAGES.set(extension, language.id);
  for (const name of language.exactNames) EXACT_NAME_LANGUAGES.set(name, language.id);
  for (const contentType of language.contentTypes) CONTENT_TYPE_LANGUAGES.set(contentType, language.id);
}

function baseContentType(contentType) {
  return String(contentType || "").split(";", 1)[0].trim().toLowerCase();
}

export function codePreviewLanguage(filename, contentType = "") {
  const name = String(filename || "").trim().replace(/\\/g, "/").split("/").at(-1)?.toLowerCase() || "";
  const exactLanguage = EXACT_NAME_LANGUAGES.get(name);
  if (exactLanguage) return exactLanguage;
  const extension = name.includes(".") ? name.split(".").at(-1) : "";
  const extensionLanguage = EXTENSION_LANGUAGES.get(extension);
  if (extensionLanguage) return extensionLanguage;
  return CONTENT_TYPE_LANGUAGES.get(baseContentType(contentType)) || null;
}

export function codePreviewCanRender(filename, contentType = "") {
  return codePreviewLanguage(filename, contentType) !== null;
}

export function codePreviewLanguages() {
  return catalog.languages.map(({ id }) => id).toSorted();
}
