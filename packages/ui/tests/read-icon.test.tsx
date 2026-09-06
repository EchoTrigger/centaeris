import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import { TaskGroupTranscriptItem } from "../src/components/chat/ToolActivityTranscript";

test.each(["running", "done"] as const)("read uses a magnifying glass while %s", (status) => {
  const html = renderToStaticMarkup(<TaskGroupTranscriptItem entry={{
    kind: "toolGroup", id: "read-icon", tasks: [{
      id: "read-icon", title: "read", summary: "", provider: "tool", status,
      normalizedInput: { path: "a.txt" },
      operations: [{ callId: "read-icon", toolName: "read", path: "a.txt", status }],
    }],
  }} />);
  expect(html).toContain("lucide-search");
});

test.each([
  ["web_search", "globe"], ["bash", "square-terminal"],
  ["write", "pencil"], ["edit", "pencil"],
  ["agent", "bot"], ["task_output", "list-checks"], ["plugin_read", "plug"],
])("%s uses the agreed action icon", (toolName, icon) => {
  const html = renderToStaticMarkup(<TaskGroupTranscriptItem entry={{
    kind: "toolGroup", id: `icon-${toolName}`, tasks: [{
      id: `icon-${toolName}`, title: toolName, summary: "", provider: "tool", status: "running",
      operations: [{ callId: `icon-${toolName}`, toolName, status: "running" }],
    }],
  }} />);
  expect(html).toContain(`lucide-${icon}`);
});
