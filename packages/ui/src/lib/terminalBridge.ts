import { invokeHost } from "../host/hostBridge";
export type TerminalSnapshot = {
	terminalId: string;
	workspaceRoot: string;
	sessionId: string | null;
	shell: string;
	state: "running" | "stopping" | "exited";
	exitCode: number | null;
	outputComplete: boolean;
	error: string | null;
};
export type TerminalTarget = { terminalId: string; serviceInstanceId: string };
export type TerminalList = {
	serviceInstanceId: string;
	terminals: TerminalSnapshot[];
};
export type TerminalPage = {
	terminal: TerminalSnapshot;
	chunks: { cursor: string; dataBase64: string }[];
	nextCursor: string;
	gap: boolean;
	hasMore: boolean;
};
function record(value: unknown, keys: string[]): Record<string, unknown> {
	if (!value || typeof value !== "object" || Array.isArray(value))
		throw Error("Invalid terminal response");
	const r = value as Record<string, unknown>;
	if (Object.keys(r).length !== keys.length || keys.some((key) => !(key in r)))
		throw Error("Unknown or missing terminal fields");
	return r;
}
export function parseTerminal(value: unknown): TerminalSnapshot {
	const r = record(value, [
		"terminalId",
		"workspaceRoot",
		"sessionId",
		"shell",
		"state",
		"exitCode",
		"outputComplete",
		"error",
	]);
	if (
		["terminalId", "workspaceRoot", "shell"].some(
			(k) => typeof r[k] !== "string",
		) ||
		!["running", "stopping", "exited"].includes(String(r.state)) ||
		(r.sessionId !== null && typeof r.sessionId !== "string") ||
		(r.exitCode !== null &&
			(!Number.isSafeInteger(r.exitCode) || Number(r.exitCode) < 0)) ||
		typeof r.outputComplete !== "boolean" ||
		(r.error !== null && typeof r.error !== "string")
	)
		throw Error("Invalid terminal snapshot");
	return r as TerminalSnapshot;
}
export function parseTerminalResponse(
	action: unknown,
	value: unknown,
): unknown {
	if (action === "start" || action === "stop") return parseTerminal(value);
	if (action === "list") {
		const r = record(value, ["serviceInstanceId", "terminals"]);
		if (typeof r.serviceInstanceId !== "string" || !Array.isArray(r.terminals))
			throw Error("Invalid terminal list");
		return { ...r, terminals: r.terminals.map(parseTerminal) };
	}
	if (action === "read") {
		const r = record(value, [
			"terminal",
			"chunks",
			"nextCursor",
			"gap",
			"hasMore",
		]);
		if (
			typeof r.nextCursor !== "string" ||
			!/^\d+$/.test(r.nextCursor) ||
			typeof r.gap !== "boolean" ||
			typeof r.hasMore !== "boolean" ||
			!Array.isArray(r.chunks)
		)
			throw Error("Invalid terminal output");
		for (const chunk of r.chunks) {
			const c = record(chunk, ["cursor", "dataBase64"]);
			if (
				typeof c.cursor !== "string" ||
				!/^\d+$/.test(c.cursor) ||
				typeof c.dataBase64 !== "string"
			)
				throw Error("Invalid terminal chunk");
		}
		return { ...r, terminal: parseTerminal(r.terminal) };
	}
	const r = record(value, ["accepted"]);
	if (r.accepted !== true) throw Error("Terminal input was not accepted");
	return r;
}
export const terminalRequest = async <T>(
	request: Record<string, unknown>,
): Promise<T> =>
	parseTerminalResponse(
		request.action,
		await invokeHost("terminal_manage", { request }),
	) as T;
export const listTerminals = (workspaceRoot: string) =>
	terminalRequest<TerminalList>({ action: "list", workspaceRoot });
export const createTerminal = (request: {
	operationId: string;
	serviceInstanceId: string;
	workspaceRoot: string;
	sessionId: string | null;
	shell: string | null;
	cols: number;
	rows: number;
}) => terminalRequest<TerminalSnapshot>({ action: "start", request });
export const readTerminal = (target: TerminalTarget, cursor: string) =>
	terminalRequest<TerminalPage>({ action: "read", target, cursor });
export const stopTerminal = (target: TerminalTarget) =>
	terminalRequest<TerminalSnapshot>({ action: "stop", target });
export const resizeTerminal = (
	target: TerminalTarget,
	cols: number,
	rows: number,
) => terminalRequest({ action: "resize", target, cols, rows });
export const writeTerminal = (target: TerminalTarget, data: Uint8Array) =>
	terminalRequest({
		action: "write",
		target,
		dataBase64: btoa(Array.from(data, (b) => String.fromCharCode(b)).join("")),
	});
