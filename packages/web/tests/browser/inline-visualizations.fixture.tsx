import { useState } from "react";
import { createRoot } from "react-dom/client";
import { MarkdownContent } from "../../src/chat/MarkdownContent";
import { previewExamples } from "../../../ui/src/visualizationPreviewExamples";
import "../../src/globals.css";

const parameters = new URLSearchParams(location.search);
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
document.body.style.overflow = "auto";
const fence = "`".repeat(3);
const visual = (source: string) => `${fence}centaeris_visual\n${source}\n${fence}`;
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));

function Fixture() {
  const [complete, setComplete] = useState(false);
  const [appended, setAppended] = useState(false);
  return <main style={{ maxWidth: 820, margin: "20px auto", padding: "0 20px", color: "var(--on-surface)" }}>
    <h1>Shared transcript visualizations</h1>
    <section aria-label="Streaming visual">
      <button type="button" onClick={() => setComplete(true)}>Complete visual</button>
      <button type="button" onClick={() => setAppended(true)}>Append prose</button>
      <MarkdownContent text={"Before stream\n\n" + (complete ? visual(previewExamples[0].source) : `${fence}centaeris_visual\n${previewExamples[0].source}`) + (appended ? "\n\nAfter stream" : "")} allowVisuals isStreaming={!complete} />
    </section>
    {previewExamples.map(example => <section key={example.id} data-example={example.id} aria-label={example.title} style={{ marginTop: 28 }}>
      <h2>{example.title}</h2>
      <MarkdownContent text={example.intro + "\n\n" + visual(example.source)} allowVisuals />
    </section>)}
    <section aria-label="Invalid visual" style={{ marginTop: 28 }}>
      <MarkdownContent text={`Before invalid\n\n${fence}centaeris_visual\n{"schema":"unsupported","html":"<script>bad()</script>"}\n${fence}\n\nAfter invalid`} allowVisuals />
    </section>
  </main>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
Object.assign(window, { visualizationFixture: { failures, examples: previewExamples.map(example => ({ id: example.id, values: Object.values(JSON.parse(example.source).spec.elements).flatMap((element: { props: Record<string, string> }) => ["title", "description", "text", "label"].flatMap(key => element.props[key] === undefined ? [] : [element.props[key]])) })) } });
