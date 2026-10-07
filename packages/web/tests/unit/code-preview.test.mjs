import assert from "node:assert/strict";
import test from "node:test";
import {
  codePreviewCanRender,
  codePreviewLanguage,
  codePreviewLanguages,
} from "../../src/chat/codePreviewFormats.mjs";
import {
  codePreviewLanguageCanLoad,
  loadCodePreviewLanguage,
} from "../../src/chat/loadCodePreviewLanguage.mjs";

test("code preview recognizes source names even when uploads use an opaque MIME type", () => {
  assert.equal(codePreviewLanguage("src/WorkspaceContextPanel.tsx", "application/octet-stream"), "tsx");
  assert.equal(codePreviewLanguage("Dockerfile", "application/octet-stream"), "dockerfile");
  assert.equal(codePreviewLanguage("query", "application/json; charset=utf-8"), "json");
});

test("code preview does not claim documents or generic binary files", () => {
  for (const filename of ["report.docx", "slides.pptx", "archive.zip", "image.png"]) {
    assert.equal(codePreviewCanRender(filename, "application/octet-stream"), false);
  }
});

test("every classified code language has a loadable syntax parser", async () => {
  const languages = codePreviewLanguages();
  assert.ok(languages.length > 20);
  for (const language of languages) assert.equal(codePreviewLanguageCanLoad(language), true, language);
  const extensions = await Promise.all(languages.map(loadCodePreviewLanguage));
  assert.equal(extensions.length, languages.length);
  assert.ok(extensions.every(Boolean));
});
