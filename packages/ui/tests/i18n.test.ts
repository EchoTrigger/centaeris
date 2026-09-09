import { expect, test } from "vitest";
import { i18n } from "../src/i18n";

test("desktop interface offers English only until Chinese is ready", () => {
  expect(i18n.resolvedLanguage).toBe("en");
  expect(i18n.options.supportedLngs).toContain("en");
  expect(i18n.options.supportedLngs).not.toContain("zh-CN");
  expect(i18n.t("reasoningTranscript.thinking")).toBe("Thinking");
  expect(i18n.t("reasoningTranscript.thoughts")).toBe("Thoughts");
});
