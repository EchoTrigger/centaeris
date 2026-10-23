import { parser } from "@lezer/markdown";
import { getNodeValue, parseTree, type Node } from "jsonc-parser";
import generated from "./generated.json" with { type: "json" };

export const catalog = generated;
export type VisualElement = {
	type: string;
	props: Record<string, string>;
	children: string[];
};
export type VisualEnvelope = {
	schema: string;
	spec: { root: string; elements: Record<string, VisualElement> };
};
export type VisualBlock = {
	start: number;
	end: number;
	json: string;
	complete: boolean;
};
const bytes = (value: string) => new TextEncoder().encode(value).length;
const object = (value: unknown): value is Record<string, unknown> =>
	typeof value === "object" && value !== null && !Array.isArray(value);
function fields(
	value: unknown,
	names: string[],
): asserts value is Record<string, unknown> {
	if (
		!object(value) ||
		Object.keys(value).some((key) => !names.includes(key)) ||
		names.some((key) => !Object.hasOwn(value, key))
	)
		throw new Error("Unsupported visualization fields");
}
function inspectJson(node: Node, depth = 0) {
	if (depth > 120) throw new Error("JSON is too deeply nested");
	if (node.type === "object") {
		const names = new Set<string>();
		for (const property of node.children ?? []) {
			const key = property.children?.[0].value;
			if (names.has(key)) throw new Error("Duplicate JSON key");
			names.add(key);
		}
	}
	for (const child of node.children ?? []) inspectJson(child, depth + 1);
}
export function parseVisual(source: string): VisualEnvelope {
	if (bytes(source) > catalog.limits.bytes)
		throw new Error("Visualization exceeds the byte limit");
	const errors: import("jsonc-parser").ParseError[] = [];
	const tree = parseTree(source, errors, {
		disallowComments: true,
		allowTrailingComma: false,
	});
	if (!tree || errors.length) throw new Error("Invalid visualization JSON");
	inspectJson(tree);
	const value: unknown = getNodeValue(tree);
	fields(value, ["schema", "spec"]);
	if (value.schema !== catalog.schema)
		throw new Error("Unsupported visualization schema");
	fields(value.spec, ["root", "elements"]);
	const { root, elements } = value.spec;
	if (
		typeof root !== "string" ||
		!object(elements) ||
		!Object.keys(elements).length ||
		Object.keys(elements).length > catalog.limits.elements
	)
		throw new Error("Invalid visualization elements");
	const rules = catalog.components as Record<
		string,
		{
			children: boolean;
			props: Record<string, { required?: boolean; enum?: string[] }>;
		}
	>;
	for (const element of Object.values(elements)) {
		fields(element, ["type", "props", "children"]);
		if (typeof element.type !== "string" || !Object.hasOwn(rules, element.type))
			throw new Error("Unknown component");
		const rule = rules[element.type];
		if (
			!object(element.props) ||
			!Array.isArray(element.children) ||
			element.children.some((id) => typeof id !== "string") ||
			(!rule.children && element.children.length)
		)
			throw new Error("Invalid component content");
		const elementProps = element.props;
		for (const [key, text] of Object.entries(elementProps)) {
			if (
				!Object.hasOwn(rule.props, key) ||
				typeof text !== "string" ||
				bytes(text) > catalog.limits.textBytes ||
				/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(text) ||
				/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(
					text,
				) ||
				(rule.props[key].enum && !rule.props[key].enum.includes(text))
			)
				throw new Error("Invalid visualization property");
		}
		for (const [key, property] of Object.entries(rule.props)) {
			if (property.required && !Object.hasOwn(elementProps, key))
				throw new Error("Missing property");
		}
	}
	const envelope = value as VisualEnvelope;
	const visited = new Set<string>();
	function visit(id: string, depth: number) {
		if (
			!id ||
			depth > catalog.limits.depth ||
			visited.has(id) ||
			!Object.hasOwn(envelope.spec.elements, id)
		)
			throw new Error("Invalid visualization tree");
		visited.add(id);
		for (const child of envelope.spec.elements[id].children)
			visit(child, depth + 1);
	}
	visit(root, 1);
	if (visited.size !== Object.keys(elements).length)
		throw new Error("Unreachable visualization elements");
	return envelope;
}
export function visualText(visual: VisualEnvelope): string {
	const lines: string[] = [];
	function walk(id: string, depth: number) {
		const element = visual.spec.elements[id],
			prefix = "  ".repeat(depth);
		for (const name of ["title", "description"]) {
			if (Object.hasOwn(element.props, name))
				lines.push(prefix + element.props[name]);
		}
		if (["Text", "Badge"].includes(element.type))
			lines.push(prefix + element.props.text);
		if (element.type === "Status")
			lines.push(
				prefix + "[" + element.props.tone + "] " + element.props.label,
			);
		if (element.type === "Divider") lines.push(prefix + "---");
		element.children.forEach((child, index) => {
			if (element.type === "Sequence") lines.push(prefix + (index + 1) + ".");
			walk(child, depth + 1);
		});
	}
	walk(visual.spec.root, 0);
	return lines.join("\n");
}
export function visualBlocks(source: string): VisualBlock[] {
	const blocks: VisualBlock[] = [];
	const tree = parser.parse(source);
	for (let node = tree.topNode.firstChild; node; node = node.nextSibling) {
		if (node.name !== "FencedCode") continue;
		const info = node.getChild("CodeInfo");
		if (!info || source.slice(info.from, info.to) !== "centaeris_visual")
			continue;
		const marks = node.getChildren("CodeMark");
		const body = node.getChild("CodeText");
		const end = source[node.to] === "\n" ? node.to + 1 : node.to;
		blocks.push({
			start: node.from,
			end,
			json: body ? source.slice(body.from, body.to) : "",
			complete: marks.length === 2,
		});
	}
	return blocks;
}
