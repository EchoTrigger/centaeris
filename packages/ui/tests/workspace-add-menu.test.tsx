import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { WorkspaceAddMenu } from "../src/components/WorkspaceAddMenu";
import { createTerminal, listTerminals } from "../src/lib/terminalBridge";
vi.mock("../src/lib/terminalBridge", () => ({
	createTerminal: vi.fn(),
	listTerminals: vi.fn(),
}));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("lost create response reuses the same operation; reopening an existing terminal does not create", async () => {
	vi.stubGlobal("document", {
		addEventListener() {},
		removeEventListener() {},
	});
	const terminal = {
		terminalId: "t",
		workspaceRoot: "/root",
		sessionId: null,
		shell: "pwsh",
		state: "running" as const,
		exitCode: null,
		outputComplete: false,
		error: null,
	};
	vi.mocked(listTerminals).mockResolvedValue({
		serviceInstanceId: "s",
		terminals: [terminal],
	});
	vi.mocked(createTerminal)
		.mockRejectedValueOnce(Error("disconnected"))
		.mockResolvedValueOnce(terminal);
	let view!: ReactTestRenderer;
	const open = vi.fn();
	await act(async () => {
		view = create(
			<WorkspaceAddMenu
				root="/root"
				sessionId={null}
				onFiles={() => {}}
				onReview={() => {}}
				onTerminal={open}
			/>,
		);
	});
	await act(async () =>
		view.root
			.findByProps({ "aria-label": "Add workspace tab" })
			.props.onClick(),
	);
	const start = () =>
		view.root
			.findAllByType("button")
			.find((b) => b.children.includes("Terminal"))!
			.props.onClick();
	await act(async () => start());
	await act(async () => start());
	expect(createTerminal).toHaveBeenCalledTimes(2);
	expect(vi.mocked(createTerminal).mock.calls[0][0]).toEqual(
		vi.mocked(createTerminal).mock.calls[1][0],
	);
	await act(async () =>
		view.root
			.findByProps({ "aria-label": "Add workspace tab" })
			.props.onClick(),
	);
	await act(async () =>
		view.root
			.findAllByType("button")
			.find((b) => b.children.includes("pwsh"))!
			.props.onClick(),
	);
	expect(createTerminal).toHaveBeenCalledTimes(2);
	expect(open).toHaveBeenLastCalledWith(terminal, "s", "pwsh 1");
	await act(async () => view.unmount());
	vi.unstubAllGlobals();
});
