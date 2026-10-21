// @vitest-environment jsdom
import {
	cleanup,
	fireEvent,
	render,
	screen,
	waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ComponentProps } from "react";
import userEvent from "@testing-library/user-event";
import { Sidebar } from "../src/components/Sidebar";

const noop = () => {};
const props: ComponentProps<typeof Sidebar> = {
	sessions: ["a", "b", "c"].map((id, sortOrder) => ({
		id,
		title: id,
		cwd: id === "b" ? "D:/other-project" : "D:/project",
		isPinned: true,
		sortOrder,
		sessionKind: "main",
		messageCount: 1,
	})),
	currentSessionId: null,
	workspaces: [],
	activeWorkspaceRoot: null,
	runningSessionIds: new Set(),
	completedSessionIds: new Set(),
	workspaceCatalogError: null,
	onNewChat: noop,
	onOpenWorkspace: noop,
	onSelectWorkspace: noop,
	onRetryWorkspaceCatalog: async () => {},
	onResetWorkspaceCatalog: async () => {},
	onSelectSession: noop,
	onRenameSession: async () => {},
	onDeleteSession: async () => {},
	onOpenResource: noop,
	onOpenFile: noop,
};
beforeEach(() => {
	vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(240);
	vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(48);
	vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
		function (this: HTMLElement) {
			const row = this.closest(".pinnedSortableRow");
			const rows = row?.parentElement?.querySelectorAll(
				".pinnedSortableRow:not([data-dnd-dragging])",
			);
			const index = row && rows ? Array.from(rows).indexOf(row) : 0;
			const translate =
				row instanceof HTMLElement
					? row.style.getPropertyValue("--dnd-translate").split(" ")
					: [];
			const top = row?.hasAttribute("data-dnd-dragging")
				? parseFloat(
						(row as HTMLElement).style.getPropertyValue("--dnd-top") || "0",
					) + parseFloat(translate[1] || "0")
				: Math.max(index, 0) * 48;
			return new DOMRect(0, top, 240, 48);
		},
	);
	vi.stubGlobal(
		"PointerEvent",
		class extends MouseEvent {
			pointerId: number;
			pointerType: string;
			isPrimary: boolean;
			constructor(type: string, init: PointerEventInit = {}) {
				super(type, init);
				this.pointerId = init.pointerId ?? 1;
				this.pointerType = init.pointerType ?? "mouse";
				this.isPrimary = init.isPrimary ?? true;
			}
		},
	);
	Object.defineProperty(Element.prototype, "setPointerCapture", {
		configurable: true,
		value: () => {},
	});
	Object.defineProperty(Element.prototype, "releasePointerCapture", {
		configurable: true,
		value: () => {},
	});
	vi.stubGlobal("matchMedia", () => ({
		matches: true,
		addEventListener() {},
		removeEventListener() {},
	}));
	vi.stubGlobal(
		"IntersectionObserver",
		class {
			observe() {}
			unobserve() {}
			disconnect() {}
		},
	);
	vi.stubGlobal(
		"ResizeObserver",
		class {
			observe() {}
			unobserve() {}
			disconnect() {}
		},
	);
	Object.defineProperty(Document.prototype, "elementFromPoint", {
		configurable: true,
		value: (_x: number, y: number) =>
			document.querySelectorAll(".pinnedSortableRow")[Math.floor(y / 48)] ??
			document.body,
	});
	Object.defineProperty(Document.prototype, "getAnimations", {
		configurable: true,
		value: () => [],
	});
	Object.defineProperty(Element.prototype, "getAnimations", {
		configurable: true,
		value: () => [],
	});
	Object.defineProperty(Element.prototype, "animate", {
		configurable: true,
		value: () => ({
			finished: Promise.resolve(),
			cancel() {},
			pause() {},
			play() {},
			addEventListener() {},
			removeEventListener() {},
		}),
	});
});
afterEach(() => {
	cleanup();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});
async function sort(
	handle: HTMLElement,
	direction: "ArrowUp" | "ArrowDown",
	cancel = false,
) {
	const user = userEvent.setup();
	handle.focus();
	await user.keyboard("[Space]");
	await waitFor(() => expect(handle.getAttribute("aria-grabbed")).toBe("true"));
	await user.keyboard(`[${direction}]`);
	await user.keyboard(cancel ? "[Escape]" : "[Space]");
}

test("keyboard sorting persists the complete pinned order and rejects a second write while saving", async () => {
	let finish!: () => void;
	const reorder = vi.fn(
		() =>
			new Promise<void>((resolve) => {
				finish = resolve;
			}),
	);
	render(<Sidebar {...props} onReorderPinnedSessions={reorder} />);
	const handle = screen.getByRole("button", { name: "Reorder b" });
	await sort(handle, "ArrowUp");
	await waitFor(() =>
		expect(reorder).toHaveBeenCalledExactlyOnceWith(["b", "a", "c"]),
	);
	expect((handle as HTMLButtonElement).disabled).toBe(true);
	expect(
		screen.getByRole("status", { name: "Pinned order" }).textContent,
	).toContain("Saving order");
	fireEvent.keyDown(handle, { key: "ArrowDown" });
	expect(reorder).toHaveBeenCalledTimes(1);
	finish();
	await waitFor(() =>
		expect((handle as HTMLButtonElement).disabled).toBe(false),
	);
});

test("a rejected reorder releases the handles for a deliberate retry", async () => {
	const reorder = vi
		.fn()
		.mockRejectedValueOnce(new Error("write failed"))
		.mockResolvedValue(undefined);
	render(<Sidebar {...props} onReorderPinnedSessions={reorder} />);
	const handle = screen.getByRole("button", { name: "Reorder b" });
	await sort(handle, "ArrowUp");
	await waitFor(() =>
		expect((handle as HTMLButtonElement).disabled).toBe(false),
	);
	expect((await screen.findByRole("alert")).textContent).toContain(
		"Order could not be saved",
	);
	await sort(handle, "ArrowDown");
	await waitFor(() => expect(reorder).toHaveBeenCalledTimes(2));
	expect(reorder).toHaveBeenLastCalledWith(["a", "c", "b"]);
	expect(screen.queryByRole("alert")).toBeNull();
});

test("partial pinned catalogs do not expose reorder handles", () => {
	render(
		<Sidebar
			{...props}
			onReorderPinnedSessions={vi.fn()}
			hasMore={(root) => root === "pinned"}
		/>,
	);
	expect(screen.queryByRole("button", { name: /Reorder/ })).toBeNull();
	expect(screen.getByRole("button", { name: "Load more" })).toBeDefined();
});

test("Escape restores the original order without writing", async () => {
	const reorder = vi.fn(async () => {});
	render(<Sidebar {...props} onReorderPinnedSessions={reorder} />);
	await sort(
		screen.getByRole("button", { name: "Reorder b" }),
		"ArrowUp",
		true,
	);
	expect(reorder).not.toHaveBeenCalled();
	expect(
		screen
			.getAllByRole("button", { name: /Reorder/ })
			.map((button) => button.getAttribute("aria-label")),
	).toEqual(["Reorder a", "Reorder b", "Reorder c"]);
});

test("pointer sorting across pinned rows persists the full new order", async () => {
	const reorder = vi.fn(async () => {});
	render(<Sidebar {...props} onReorderPinnedSessions={reorder} />);
	const handle = screen.getByRole("button", { name: "Reorder a" });
	fireEvent.pointerDown(handle, {
		button: 0,
		buttons: 1,
		pointerId: 1,
		pointerType: "mouse",
		isPrimary: true,
		clientX: 20,
		clientY: 24,
	});
	await waitFor(() => expect(handle.getAttribute("aria-grabbed")).toBe("true"));
	fireEvent.pointerMove(document, {
		pointerId: 1,
		pointerType: "mouse",
		isPrimary: true,
		clientX: 20,
		clientY: 120,
	});
	await waitFor(() =>
		expect(
			screen
				.getAllByRole("button", { name: /Reorder/ })
				.map((button) => button.getAttribute("aria-label")),
		).toEqual(["Reorder b", "Reorder c", "Reorder a"]),
	);
	fireEvent.pointerUp(document, {
		pointerId: 1,
		pointerType: "mouse",
		isPrimary: true,
		clientX: 20,
		clientY: 120,
	});
	await waitFor(() =>
		expect(reorder).toHaveBeenCalledExactlyOnceWith(["b", "c", "a"]),
	);
});

test("a catalog update during a drag prevents a stale full-order write", async () => {
	const reorder = vi.fn(async () => {});
	const view = render(<Sidebar {...props} onReorderPinnedSessions={reorder} />);
	const handle = screen.getByRole("button", { name: "Reorder b" });
	const user = userEvent.setup();
	handle.focus();
	await user.keyboard("[Space]");
	await waitFor(() => expect(handle.getAttribute("aria-grabbed")).toBe("true"));
	await user.keyboard("[ArrowUp]");
	view.rerender(
		<Sidebar
			{...props}
			sessions={[
				...props.sessions,
				{ ...props.sessions[0], id: "d", title: "d", sortOrder: 3 },
			]}
			onReorderPinnedSessions={reorder}
		/>,
	);
	await user.keyboard("[Space]");
	expect(reorder).not.toHaveBeenCalled();
	await waitFor(() =>
		expect(screen.getAllByRole("button", { name: /Reorder/ })).toHaveLength(4),
	);
});
