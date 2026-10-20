import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import {
	parseVisual,
	visualText,
} from "@centaeris/visualization-react/contract";
import { MarkdownContent } from "./components/chat/MarkdownContent";
import examples from "../../core/tests/fixtures/visualizations.json";
import "./index.css";
import "./styles/visualization-preview.css";

const fence = String.fromCharCode(96).repeat(3);
function Preview() {
	const [dark, setDark] = useState(true);
	const [narrow, setNarrow] = useState(false);
	const [state, setState] = useState("complete");
	useEffect(() => {
		document.documentElement.dataset.theme = dark ? "dark" : "light";
	}, [dark]);
	return (
		<main
			className="visualizationPreview"
			style={{ maxWidth: narrow ? 420 : 1100 }}
		>
			<header>
				<div>
					<small>CENTAERIS · DESIGN PREVIEW</small>
					<h1>内联图示</h1>
					<p>Desktop / Web 共用组件，TUI 保留相同的阅读顺序。</p>
				</div>
				<div className="visualizationPreviewControls">
					<label>
						<input
							type="checkbox"
							checked={dark}
							onChange={(event) => setDark(event.target.checked)}
						/>
						深色
					</label>
					<label>
						<input
							type="checkbox"
							checked={narrow}
							onChange={(event) => setNarrow(event.target.checked)}
						/>
						窄布局
					</label>
					<select
						aria-label="内容状态"
						value={state}
						onChange={(event) => setState(event.target.value)}
					>
						<option value="complete">完整内容</option>
						<option value="streaming">生成中</option>
						<option value="interrupted">生成中断</option>
						<option value="invalid">无效描述</option>
					</select>
				</div>
			</header>
			{examples.slice(0, 2).map((example, index) => {
				let json = example.source;
				if (state === "invalid")
					json = json.replace(
						"centaeris.visual.v1",
						"centaeris.visual.unknown",
					);
				const source =
					"下面的图示说明" +
					(index ? "资源控制链路。" : "同一个 Session 中的两次 Run。") +
					"\n\n" +
					fence +
					"centaeris_visual\n" +
					json +
					(state === "complete" || state === "invalid" ? "\n" + fence : "");
				return (
					<section key={example.name}>
						<h2>{index ? "资源控制链路" : "Session 生命周期"}</h2>
						<div className="answerMarkdownBlock">
							<MarkdownContent
								text={source}
								allowVisuals
								isStreaming={state === "streaming"}
							/>
						</div>
						<details>
							<summary>TUI 文本投影</summary>
							<pre>{visualText(parseVisual(example.source))}</pre>
						</details>
					</section>
				);
			})}
		</main>
	);
}
document.body.classList.add("visualizationPreviewPage");
createRoot(document.getElementById("root")!).render(<Preview />);
