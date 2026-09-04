import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("keeps the virtual message list architecture", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const virtualListSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "VirtualMessageList.tsx"),
    "utf8",
  );

  assert.match(
    virtualListSource,
    /useVirtualizer\(\{/,
    "VirtualMessageList must use TanStack Virtual",
  );
});
