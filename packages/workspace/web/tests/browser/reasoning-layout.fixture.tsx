import { createRoot } from "react-dom/client";
import { useState } from "react";
import { ReasoningBlock } from "../../src/chat/ReasoningBlock";
import { i18n } from "../../src/i18n";
import "../../src/globals.css";

const parameters = new URLSearchParams(location.search);
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
await i18n.changeLanguage(parameters.get("language") ?? "zh-CN");
const longText = "首句保持可读。First visible words remain readable while a long reasoning preview explains next steps, checks requested files, and keeps its disclosure arrow beside the title.";
document.body.style.minHeight = "100vh";
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
let append = () => {};
function Fixture() {
  const [expanded, setExpanded] = useState(false);
  const [stream, setStream] = useState("核对输入。Check inputs.");
  append = () => setStream(longText);
  return <main style={{ width: "min(100% - 32px, 820px)", margin: "40px auto", display: "grid", gridTemplateColumns: "minmax(0, 1fr)", gap: 24 }}>
    <section data-case="short"><ReasoningBlock block={{ id: "short", sequence: 1, status: "done", text: "核对输入。Check inputs." }} expanded={false} onToggle={() => {}} /></section>
    <section data-case="long"><ReasoningBlock block={{ id: "long", sequence: 2, status: "done", text: longText }} expanded={expanded} onToggle={() => setExpanded(value => !value)} /></section>
    <section data-case="stream"><ReasoningBlock block={{ id: "stream", sequence: 3, status: "streaming", text: stream }} expanded={false} onToggle={() => {}} /></section>
  </main>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
Object.assign(window, { reasoningLayoutFixture: { failures, append: () => append(), longText } });
