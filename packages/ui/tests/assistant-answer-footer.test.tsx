// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { VirtualMessageList } from "../src/components/chat/VirtualMessageList";
import { useChatViewStore } from "../src/components/chat/chatViewStore";
import type { ChatMessage } from "../src/components/chat/types";
vi.mock("@tanstack/react-virtual", () => ({
	useVirtualizer: ({ count }: { count: number }) => ({
		getTotalSize: () => count * 240,
		getVirtualItems: () =>
			Array.from({ length: count }, (_, index) => ({ index, start: index * 240 })),
		measurementsCache: [],
		measureElement: () => {},
		resizeItem: () => {},
	}),
}));
vi.mock("../src/components/chat/AgentResultStream", () => ({
	AgentResultStream: () => <div>Rendered answer</div>,
}));
afterEach(() => {
	cleanup();
	useChatViewStore.getState().replaceMessages([]);
});
const finished = new Date("2026-10-22T08:12:00Z").getTime();
const answer = (isStreaming = false): ChatMessage => ({
	id: "msg:assistant:turn-1",
	role: "assistant",
	turn: {
		id: "turn-1",
		projectionRunId: "run-1",
		chunks: [],
		finalAnswer: "**Final answer**",
		isStreaming,
	},
});
function mount(messages: ChatMessage[]) {
	useChatViewStore.getState().replaceMessages(messages);
	const noop = () => {};
	return render(
		<VirtualMessageList
			containerRef={createRef()}
			editingUserMessageId={null}
			editingPrompt=""
			copiedUserMessageId={null}
			latestUserMessageId={null}
			editableUserMessageId={null}
			onScroll={noop}
			onContentSizeChange={noop}
			onEditingPromptChange={noop}
			onCancelEditingUserMessage={noop}
			onSubmitEditedUserMessage={noop}
			onCopyUserMessage={noop}
			onStartEditingUserMessage={noop}
		/>,
	);
}
test("historical final answer copies only the complete answer and shows the authoritative completion time", async () => {
	const user = userEvent.setup();
	const copy = vi.spyOn(navigator.clipboard, "writeText");
	const { container } = mount([
		{
			id: "process",
			role: "assistant",
			turn: {
				id: "process",
				projectionRunId: "run-1",
				finalAnswer: "",
				isStreaming: false,
				chunks: [{ id: "note", kind: "narrative", text: "Private process" }],
			},
		},
		answer(),
		{
			id: "end",
			role: "assistant",
			turn: {
				id: "end",
				projectionRunId: "run-1",
				chunks: [],
				finalAnswer: "",
				isStreaming: false,
				completedAtMs: finished,
			},
		},
	]);
	await user.click(screen.getByRole("button", { name: "Copy answer" }));
	expect(copy).toHaveBeenCalledExactlyOnceWith("**Final answer**");
	expect(screen.getByRole("button", { name: "Copied answer" })).toBeTruthy();
	expect(container.querySelector("time")?.dateTime).toBe(new Date(finished).toISOString());
});
test("streaming has no answer actions and missing completion time is never fabricated", async () => {
	const message = answer(true);
	const { container } = mount([message]);
	expect(screen.queryByRole("button", { name: "Copy answer" })).toBeNull();
	await act(async () => useChatViewStore.getState().replaceMessages([answer()]));
	expect(screen.getByRole("button", { name: "Copy answer" })).toBeTruthy();
	expect(container.querySelector("time")).toBeNull();
});
