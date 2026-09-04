import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("keeps the chat composer isolated", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const chatAreaSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "ChatArea.tsx"),
    "utf8",
  );
  const chatComposerSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "ChatComposer.tsx"),
    "utf8",
  );

  assert.match(
    chatComposerSource,
    /import \{[^}]*memo[^}]*useEffect[^}]*useRef[^}]*\} from "react"/,
    "ChatComposer must import React memo",
  );
  assert.match(
    chatComposerSource,
    /export const ChatComposer = memo\(function ChatComposer/,
    "ChatComposer must be memoized at the component boundary",
  );
  assert.match(
    chatComposerSource,
    /nativeEvent\.isComposing/,
    "ChatComposer must not submit while an IME composition is active",
  );
  assert.match(
    chatComposerSource,
    /nativeEvent\.keyCode === 229/,
    "ChatComposer must recognize the legacy IME composition key code",
  );
  assert.match(
    chatComposerSource,
    /onCompositionStart=/,
    "ChatComposer must record the start of an IME composition",
  );
  assert.match(
    chatComposerSource,
    /onCompositionEnd=/,
    "ChatComposer must record the end of an IME composition",
  );
  assert.match(
    chatComposerSource,
    /COMPOSITION_END_ENTER_GRACE_MS = 100/,
    "ChatComposer must suppress the trailing Enter emitted after composition ends",
  );
  assert.match(
    chatComposerSource,
    /requestAnimationFrame\(\(\) => textareaRef\.current\?\.focus\(\)\)/,
    "ChatComposer must restore textarea focus after a pointer send action",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /<ChatComposer[\s\S]*messages=/,
    "ChatComposer must not receive message stream state as props",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /const\s*\[\s*messages\s*,\s*setMessages\s*\]\s*=\s*useState/,
    "message stream updates must not be ChatArea React state",
  );
  assert.match(
    chatAreaSource,
    /const setMessages = useCallback\(/,
    "ChatArea may keep a compatibility setter only as a ref/store commit wrapper",
  );
  assert.match(
    chatAreaSource,
    /scheduleVisibleSessionViewCachePersistRef\.current\?\.\(\)/,
    "message commits must trigger cache persistence explicitly without a messages effect",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /}, \[messages\]\)/,
    "ChatArea must not use messages as an effect dependency",
  );
});
