import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("session hydration failure replaces cached projection with an error page", async () => {
  const source = await readFile(
    path.resolve(import.meta.dirname, "..", "src", "components", "chat", "ChatArea.tsx"),
    "utf8",
  );
  const failureBranch = source.slice(
    source.indexOf("} catch (error) {", source.indexOf("const hydrateSession")),
    source.indexOf("void hydrateSession()"),
  );

  assert.match(failureBranch, /sessionViewCacheStore\.delete\(currentSessionId\)/);
  assert.match(failureBranch, /setMessages\(\[\]\)/);
  assert.match(failureBranch, /setSessionLoadError\(formatExecutionError\(error\)\)/);
  assert.match(source, /data-chat-view-mode="error"[\s\S]*role="alert"/);
  assert.match(source, /\{!sessionLoadError \? \([\s\S]*className=\{`chatBottomPlane/);
});
