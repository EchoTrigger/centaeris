import {
	parseVisual,
	visualText,
	type VisualElement,
	type VisualEnvelope,
} from "@centaeris/visualization-react/contract";
import fixtures from "../../core/tests/fixtures/visualizations.json";

// Samples exercise the production catalog, Markdown path and Desktop renderer.
const session = parseVisual(fixtures[0].source);
const sessionElements = session.spec.elements;
sessionElements.root.props = {};
sessionElements.flow.children = ["runA", "runB"];
sessionElements.runA.children.push("retained");
sessionElements.ended.props.label = "Run A 结束，其 Run-scoped 任务被清理";
sessionElements.retained = {
	type: "Group",
	props: { title: sessionElements.retained.props.text },
	children: ["environmentIcon"],
};
sessionElements.environmentIcon = {
	type: "Icon",
	props: { name: "hardDrive" },
	children: [],
};

const resources = parseVisual(fixtures[1].source);
resources.spec.elements.root.props = {};
resources.spec.elements.container.children.unshift("branchIcon");
resources.spec.elements.branchIcon = {
	type: "Icon",
	props: { name: "gitBranch" },
	children: [],
};

type SampleNode = {
	type: string;
	props: Record<string, string>;
	children: SampleNode[];
};
function node(
	type: string,
	props: Record<string, string>,
	children: SampleNode[] = [],
): SampleNode {
	return { type, props, children };
}
function group(title: string, description: string, children: SampleNode[] = []) {
	return node("Group", { title, description }, children);
}
function status(tone: string, label: string) {
	return node("Status", { tone, label });
}
function card(children: SampleNode[]): VisualEnvelope {
	const elements: Record<string, VisualElement> = {};
	let next = 0;
	function visit(sample: SampleNode): string {
		const id = "sample" + next++;
		elements[id] = { type: sample.type, props: sample.props, children: [] };
		elements[id].children = sample.children.map(visit);
		return id;
	}
	const root = visit(node("Card", {}, children));
	return { schema: session.schema, spec: { root, elements } };
}

const delivery = card([
	node("Sequence", {}, [
		group("理解需求", "明确改动入口、约束和验收标准。", [
			status("success", "需求与验收方式已明确"),
		]),
		group("实现与说明", "在工作树内完成修改，让说明与行为保持一致。", [
			status("success", "修改与说明已准备"),
		]),
		group("本地验证", "运行相关检查，人工确认交互与外观。", [
			status("info", "正在核对验证结果"),
		]),
		group("交付审阅", "附上变更内容、验证结果和仍需确认的事项。", [
			node("Badge", { text: "待审阅" }),
		]),
	]),
]);
const recovery = card([
	node("Sequence", {}, [
		group("执行中断", "工具连接断开，当前步骤未能完成。", [
			status("error", "本次执行未完成"),
		]),
		group("保留上下文", "保留已经收到的输出和工作文件。", [
			status("neutral", "等待恢复连接"),
		]),
		group("确认后继续", "连接恢复后，先确认上一步结果，再决定是否重试。", [
			status("warning", "结果未确认前，避免重复执行"),
		]),
	]),
]);
const ownership = card([
	group("Session", "会话上下文与工作记录", [
		group("执行环境", "文件、进程与资源约束", [
			node("Columns", {}, [
				group("Task A", "安装依赖", [
					status("success", "已完成"),
				]),
				group("Task B", "运行构建", [
					status("info", "进行中"),
				]),
			]),
		]),
	]),
]);
const longText = card([
	node("Sequence", {}, [
		group(
			"检查依赖与构建配置",
			"说明中同时出现中文、English identifiers 和文件路径时，文字应自然换行，状态仍然跟着所属步骤阅读。",
			[
				node("Text", {
					text: "packages/ui/src/components/chat/useTranscriptText.tsx",
					role: "caption",
				}),
				status("warning", "依赖尚未就绪时，先定位原因，再重试构建。"),
			],
		),
		group(
			"确认结果与下一步",
			"较长的补充说明留在当前步骤内部，不拆成更多小卡片，也不挤到旁边形成另一条阅读路径。",
			[
				status("info", "保留错误摘要与复现命令，方便继续排查。"),
			],
		),
	]),
]);

export const previewExamples = [
	{
		id: "session",
		title: "Session 生命周期",
		intro: "假设一个 Session 包含连续两次 AgentRun：",
		visual: session,
	},
	{
		id: "resources",
		title: "资源控制链路",
		intro: "从资源预算到任务约束，沿着同一方向逐层向下：",
		visual: resources,
	},
	{
		id: "delivery",
		title: "交付流程",
		intro: "一个功能从需求到审阅的流程示例：",
		visual: delivery,
	},
	{
		id: "recovery",
		title: "异常恢复",
		intro: "用一次连接中断，看看错误、保留状态与后续动作如何衔接：",
		visual: recovery,
	},
	{
		id: "ownership",
		title: "任务归属",
		intro: "上下列出的 Task A / B 属于同级任务，不表示执行先后：",
		visual: ownership,
	},
	{
		id: "longText",
		title: "长文本与路径",
		intro: "较长的文字也应顺着正文阅读，以下是一个排查过程的示例：",
		visual: longText,
	},
].map(({ visual, ...example }) => {
	const source = JSON.stringify(visual);
	const validated = parseVisual(source);
	return { ...example, source, projection: visualText(validated) };
});
