import { createRoot } from "react-dom/client";
import { ThemeSelector } from "../../src/components/ThemeSelector";
import { CodePreview } from "../../src/components/CodePreview";
import { ReasoningTranscript } from "../../src/components/chat/ReasoningTranscript";
import { MarkdownContent } from "../../src/components/chat/MarkdownContent";
import "../../src/index.css";

const root = document.getElementById("root");
if (!root) throw new Error("fixture root missing");
createRoot(root).render(<main style={{ maxWidth: 868, padding: 24, margin: "auto" }}>
  <ThemeSelector />
  <MarkdownContent text={"Theme changes preserve the current conversation and its contents.\n\n正文使用 14px，工具标题与思考标题同样为 14px；展开的内容和表格使用 13px。\n\n| Item | Description |\n| --- | --- |\n| Theme | System default, dark, or light |\n| Long path | packages/ui/src/components/chat/ReasoningTranscript.tsx |"} />
  <ReasoningTranscript scopeId="appearance" entry={{ id: "r1", kind: "reasoning", status: "done", text: "思考标题与详情使用同一种灰色。\n\nKeep long explanations readable without changing their content. `path/to/file.ts`" }} />
  <section aria-label="Code preview" style={{ height: 240, marginTop: 24 }}>
    <CodePreview content={'// Preserve editor state across theme changes\nconst language = "English";\nconst attempts = 3;\nfunction run() { return language; }'} path="example.ts" />
  </section>
</main>);
