import { expect, test } from "vitest";
import { readFile } from "node:fs/promises";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { buildCustomProvidersInput } from "../src/components/ModelsDialog.tsx";
import { ModelsDialog } from "../src/components/ModelsDialog.tsx";

test("provider catalog is not the Models landing page", () => {
  const markup = renderToStaticMarkup(createElement(ModelsDialog, { onClose() {} }));

  expect(markup).toContain("Select a provider");
  expect(markup).toContain("Add provider");
  expect(markup).not.toContain("OAUTH SUBSCRIPTIONS");
});

test("fixed catalog providers stay API-only in settings", async () => {
  const source = await readFile(new URL("../src/components/ModelsDialog.tsx", import.meta.url), "utf8");
  const fixedProviderTree = source.slice(
    source.indexOf("{visibleBuiltIns.map"),
    source.indexOf("{customProviders.map"),
  );

  expect(fixedProviderTree).not.toContain("modelsProviderModels");
  expect(source).not.toContain("modelsBuiltInModel");
  expect(source).not.toContain("First-party HTTPS endpoint");
});

test("model settings preserve incomplete provider and model drafts", () => {
  const providers = [{
    providerId: "custom.test",
    name: "",
    baseUrl: "banana",
    api: "openai-completions",
    models: [{
      key: 1,
      model: "",
      displayName: "",
      contextTokens: "banana",
      maxOutputTokens: "",
    }],
  }];

  expect(buildCustomProvidersInput(providers)).toEqual([{
    providerId: "custom.test",
    name: "",
    baseUrl: "banana",
    api: "openai-completions",
    models: [{
      model: "",
      displayName: undefined,
      contextTokens: "banana",
      maxOutputTokens: "",
      apiOverride: undefined,
    }],
  }]);
});
