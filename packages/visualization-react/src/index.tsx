import {
	Component,
	Fragment,
	lazy,
	memo,
	Suspense,
	useMemo,
	type ReactNode,
} from "react";
import { parseVisual, visualBlocks, type VisualBlock, type VisualEnvelope } from "./contract";
import "./styles.css";

const LazyRenderer = lazy(() =>
	import("./Renderer").then((module) => ({ default: module.VisualRenderer })),
);
const pending = (
	<div className="cv-pending" role="status">
		Loading visualization…
	</div>
);

function SourceError({ reason, source }: { reason?: string; source: string }) {
	return (
		<div className="cv-error">
			<p role="status">{reason}</p>
			<details>
				<summary>Original source</summary>
				<pre>{source}</pre>
			</details>
		</div>
	);
}

class VisualBoundary extends Component<
	{ source: string; children: ReactNode },
	{ failed: boolean }
> {
	state = { failed: false };
	static getDerivedStateFromError() {
		return { failed: true };
	}
	render() {
		return this.state.failed ? (
			<SourceError
				reason="Unable to display visualization"
				source={this.props.source}
			/>
		) : (
			this.props.children
		);
	}
}

type RenderVisual = (visual: VisualEnvelope) => ReactNode;

function HostVisual({ visual, renderVisual }: { visual: VisualEnvelope; renderVisual: RenderVisual }) {
  return renderVisual(visual);
}


const Visual = memo(
	function Visual({
		block,
		source,
		finalized,
		renderVisual,
	}: {
		block: VisualBlock;
		source: string;
		finalized: boolean;
		renderVisual?: RenderVisual;
	}) {
		const result = useMemo(() => {
			if (!block.complete) return { error: "Incomplete visualization" };
			try {
				return { visual: parseVisual(block.json) };
			} catch (error) {
				return {
					error:
						error instanceof Error ? error.message : "Invalid visualization",
				};
			}
		}, [block.complete, block.json]);
		if (!block.complete && !finalized) return pending;
		if (result.visual)
			return (
				<VisualBoundary key={block.json} source={source}>
					<Suspense fallback={pending}>
						{renderVisual ? <HostVisual visual={result.visual} renderVisual={renderVisual} /> : <LazyRenderer visual={result.visual} />}
					</Suspense>
				</VisualBoundary>
			);
		return <SourceError reason={result.error} source={source} />;
	},
	(before, after) =>
		before.source === after.source && before.finalized === after.finalized && before.renderVisual === after.renderVisual,
);

export function VisualMarkdown({
	text,
	finalized,
	renderMarkdown,
	renderVisual,
}: {
	text: string;
	finalized: boolean;
	renderMarkdown: (source: string) => ReactNode;
	renderVisual?: RenderVisual;
}) {
	const blocks = useMemo(() => visualBlocks(text), [text]);
	const nodes: ReactNode[] = [];
	let cursor = 0;
	for (const block of blocks) {
		if (block.start > cursor)
			nodes.push(
				<Fragment key={"text-" + cursor}>
					{renderMarkdown(text.slice(cursor, block.start))}
				</Fragment>,
			);
		nodes.push(
			<Visual
				key={"visual-" + block.start}
				block={block}
				source={text.slice(block.start, block.end)}
				finalized={finalized}
				renderVisual={renderVisual}
			/>,
		);
		cursor = block.end;
	}
	if (cursor < text.length)
		nodes.push(
			<Fragment key={"text-" + cursor}>
				{renderMarkdown(text.slice(cursor))}
			</Fragment>,
		);
	return <>{nodes}</>;
}
export { parseVisual, visualBlocks, visualText } from "./contract";
