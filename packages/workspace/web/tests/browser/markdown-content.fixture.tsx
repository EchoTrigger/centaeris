import { useState } from "react";
import { createRoot } from "react-dom/client";
import { ChevronDown } from "lucide-react";
import visualCases from "../../../../core/tests/fixtures/visualizations.json";
import { MarkdownContent } from "../../src/chat/MarkdownContent";
import { TranscriptSearchQuery } from "../../src/chat/TranscriptSearchHighlight";
import { changeTheme } from "../../src/theme";
import { i18n } from "../../src/i18n";
import "../../src/globals.css";

const parameters = new URLSearchParams(location.search);
const theme = parameters.get("theme") === "dark" ? "dark" : "light";
document.documentElement.dataset.theme = theme;
changeTheme(theme);
void i18n.changeLanguage("en");
document.body.style.overflow = "auto";
const fence = String.fromCharCode(96).repeat(3);
const code = 'const message = "流式🙂";\nconsole.log(message);';
const diagram = "graph TD\n  A[Start] --> B[Done]";
const extendedDiagram = diagram + "\n  B --> C[Next]";
const invalidDiagram = extendedDiagram + "\n  C --> D[";
const replacementDiagram = "graph TD\n  X[Replacement] --> Y[Ready]";
const failures: string[] = [];
const copied: string[] = [];
let workerReceipts = 0, workerTerminations = 0;
const workerUrls: string[] = [], workerErrors: string[] = [];
const timings: Record<string, number> = {};
const BrowserWorker = window.Worker;
window.Worker = class ObservedWorker extends BrowserWorker {
  constructor(url: string | URL, options?: WorkerOptions) {
    super(url, options);
    workerUrls.push(String(url));
    this.addEventListener("message", () => { workerReceipts++; });
    this.addEventListener("error", event => { workerErrors.push(event.message); });
  }
  terminate() { workerTerminations++; super.terminate(); }
};
const longPrefix = "## Long response\n\n", longSuffix = "\n\nEntity &copy; &amp;.\n\n**Long response complete.**";
const longLength = 1024 * 1024 - longPrefix.length - longSuffix.length;
const longSource = longPrefix + "alpha ".repeat(Math.ceil(longLength / 6)).slice(0, longLength) + longSuffix;
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
  async writeText(value: string) { copied.push(value); },
} });
const astSource = [
  "## Nested content", "", "- Parent", "  - Child **bold**", "    - Grandchild", "",
  "[Guide][doc]", "", "| Item | Value |", "| --- | --- |", "| a\\|b | **yes** |", "",
  "- [x] Complete", "- [ ] Pending", "", fence + "centaeris_visual", visualCases[0].source, fence, "",
  "[doc]: https://example.com/docs", "", "<script>window.markdownUnsafe = true</script>",
].join("\n");

function Fixture() {
  const [stage, setStage] = useState<"valid" | "extended" | "incomplete" | "invalid" | "replacement">("valid");
  const [jumped, setJumped] = useState(false);
  const [longText, setLongText] = useState("Initial response.");
  const [longStreaming, setLongStreaming] = useState(false);
  const [draft, setDraft] = useState("");
  const [readingActions, setReadingActions] = useState(0);
  const currentDiagram = stage === "replacement" ? replacementDiagram
    : stage === "incomplete" || stage === "invalid" ? invalidDiagram
      : stage === "extended" ? extendedDiagram : diagram;
  return <main style={{ maxWidth: 880, margin: "20px auto", padding: "0 20px", color: "var(--on-surface)", display: "grid", gap: 28 }}>
    <h1>Markdown reading acceptance</h1>
    <section aria-label="AST example"><MarkdownContent text={astSource} allowVisuals /></section>
    <section aria-label="Code example">
      <TranscriptSearchQuery.Provider value="message =">
        <MarkdownContent text={fence + "typescript\n" + code + "\n" + fence} />
      </TranscriptSearchQuery.Provider>
    </section>
    <section aria-label="Math example">
      <MarkdownContent text={"\\(E=mc^2\\)\n\n$$\n\\frac{a}{b}\n$$\n\nCost $5 and $10."} />
    </section>
    <section aria-label="Diagram example">
      <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
        <button type="button" onClick={() => setStage("extended")}>Extend valid diagram</button>
        <button type="button" onClick={() => setStage("incomplete")}>Append incomplete diagram</button>
        <button type="button" onClick={() => setStage("invalid")}>Finish invalid diagram</button>
        <button type="button" onClick={() => setStage("replacement")}>Replace diagram</button>
      </div>
      <MarkdownContent text={fence + "mermaid\n" + currentDiagram + "\n" + fence} isStreaming={stage === "extended" || stage === "incomplete"} />
    </section>
    <section aria-label="Return control example" style={{ position: "relative", minHeight: 112 }}>
      <button type="button" id="before-return">Before return control</button>
      <button type="button" className="workspaceJumpToLatest" aria-label="Return to latest" onClick={() => setJumped(true)}><ChevronDown aria-hidden="true" /></button>
      <p role="status">{jumped ? "Returned to latest" : "Reading earlier content"}</p>
    </section>
    <section aria-label="Long response example">
      <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
        <button type="button" onClick={() => { timings.longStarted = performance.now(); setLongText(longSource); setLongStreaming(true); }}>Load long response</button>
        <button type="button" onClick={() => setLongStreaming(false)}>Finalize long response</button>
        <button type="button" onClick={() => { setLongText(longSource.replace("Long response", "Revised response")); setLongStreaming(true); }}>Start revised long response</button>
        <button type="button" onClick={() => { setLongText("**Corrected final response.**"); setLongStreaming(false); }}>Replace long response</button>
        <button type="button" onClick={event => { const at = event.timeStamp; setReadingActions(value => value + 1); requestAnimationFrame(() => { timings.actionFrameMs = performance.now() - at; }); }}>Record reading action</button>
      </div>
      <label>Draft while reading<input aria-label="Draft while reading" value={draft} onChange={event => { const at = event.timeStamp; setDraft(event.target.value); requestAnimationFrame(() => { timings.inputFrameMs = performance.now() - at; }); }} /></label>
      <output aria-label="Reading action count">{readingActions}</output>
      <MarkdownContent text={longText} isStreaming={longStreaming} />
    </section>
  </main>;
}

Object.assign(window, { markdownFixture: { failures, copied, timings, workerUrls, workerErrors, get workerReceipts() { return workerReceipts; }, get workerTerminations() { return workerTerminations; } } });
createRoot(document.getElementById("fixture")!).render(<Fixture />);
