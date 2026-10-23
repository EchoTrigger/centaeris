import { MotionConfig } from "motion/react";
import { TooltipProvider } from "./components/ui/tooltip";
import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { MarkdownContent } from "./components/chat/MarkdownContent";
import { previewExamples } from "./visualizationPreviewExamples";
import "./index.css";
import "./styles/visualization-preview.css";

const fence = String.fromCharCode(96).repeat(3);
const containmentExamples = new Set(["resources", "ownership"]);

function PreviewExample({
	example,
}: {
	example: (typeof previewExamples)[number];
}) {
	const source =
		example.intro + "\n\n" + fence + "centaeris_visual\n" +
		example.source + "\n" + fence;

	return (
		<section className="visualizationExample" data-example={example.id}
			aria-labelledby={example.id + "-heading"}>
			<h2 id={example.id + "-heading"}>{example.title}</h2>
			<div className="answerMarkdownBlock">
				<MarkdownContent text={source} allowVisuals />
			</div>
			<details className="previewProjection">
				<summary>TUI 文本投影</summary>
				<pre>{example.projection}</pre>
			</details>
		</section>
	);
}

function Preview() {
	const [dark, setDark] = useState(false);
	const [narrow, setNarrow] = useState(false);
	const [selected, setSelected] = useState(() => {
		const requested = new URLSearchParams(window.location.search).get("example");
		return requested === "containment"
			? "containment"
			: previewExamples.find((item) => item.id === requested)?.id ?? "all";
	});
	useEffect(() => {
		document.documentElement.dataset.theme = dark ? "dark" : "light";
	}, [dark]);

	const visibleExamples = previewExamples.filter((item) =>
		selected === "all" || item.id === selected ||
		(selected === "containment" && containmentExamples.has(item.id)),
	);

	return (
		<main className="visualizationPreview">
			<header className="previewHeader">
				<h1>内联图示</h1>
				<p>参考图的组织方式，收紧留白，按正文的顺序阅读。</p>
				<div className="visualizationPreviewControls">
					<label>
						查看示例
						<select value={selected} onChange={(event) => setSelected(event.target.value)}>
							<option value="all">全部六个参考</option>
							<option value="containment">仅看两种嵌套参考</option>
							{previewExamples.map((example) => (
								<option key={example.id} value={example.id}>{example.title}</option>
							))}
						</select>
					</label>
					<label>
						<input type="checkbox" checked={dark}
							onChange={(event) => setDark(event.target.checked)} />
						深色
					</label>
					<label>
						<input type="checkbox" checked={narrow}
							onChange={(event) => setNarrow(event.target.checked)} />
						窄布局
					</label>
				</div>
			</header>
			<div className="previewExamples" data-narrow={narrow}>
				{visibleExamples.map((example) => (
					<PreviewExample key={example.id} example={example} />
				))}
			</div>
		</main>
	);
}

document.body.classList.add("visualizationPreviewPage");
createRoot(document.getElementById("root")!).render(<MotionConfig reducedMotion="user"><TooltipProvider><Preview /></TooltipProvider></MotionConfig>);
