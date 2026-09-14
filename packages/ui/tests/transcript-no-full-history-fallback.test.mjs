import { readFileSync } from "node:fs";
import { expect, test } from "vitest";

const readSource = (relativePath) => readFileSync(
  new URL(relativePath, import.meta.url),
  "utf8",
);

test("the Desktop transcript renderer has no legacy full-history RPC fallback", () => {
  const hydrationController = readSource(
    "../src/components/chat/useSessionViewHydrationController.ts",
  );
  const runtimeModel = readSource(
    "../src/components/chat/chatRuntimeModel.ts",
  );

  expect(hydrationController).not.toMatch(/\bgetSession\b/);
  expect(hydrationController).not.toMatch(/replayAgentRunStreamFromCursor/);
  expect(runtimeModel).not.toMatch(/\bgetSessionProjection\b/);
  expect(runtimeModel).not.toMatch(/buildLegacySessionHydrationSnapshot/);
});
