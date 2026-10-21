// @vitest-environment jsdom
import {
	act,
	cleanup,
	render,
	screen,
	waitFor,
	within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { DesktopFilePreviewReadResponse } from "../src/lib/workspaceBridge";
import type { SummaryPanelTab } from "../src/components/SummaryPanel";
import { getWorkspaceGitStatus } from "../src/lib/workspaceBridge";

type PreviewRequest = {
	path: string;
	promise: Promise<DesktopFilePreviewReadResponse>;
	resolve: (response: DesktopFilePreviewReadResponse) => void;
};

type ChatHarnessProps = {
	onOpenWorkspacePath: (
		path: string,
		options?: { startLine?: number; endLine?: number; taskId?: string },
	) => Promise<void>;
	onOpenAgentSession: (sessionId: string, title: string) => void;
};

type SummaryHarnessProps = {
	tabs: SummaryPanelTab[];
	activeTabId: string | null;
	onCloseTab: (tabId: string) => void;
	onCollapse: () => void;
};

const harness = vi.hoisted(() => ({
	chatProps: null as ChatHarnessProps | null,
	summaryProps: null as SummaryHarnessProps | null,
	previewRequests: [] as PreviewRequest[],
}));

vi.mock("../src/components/Sidebar", () => ({
	Sidebar: () => <aside data-testid="sidebar" />,
}));

vi.mock("../src/components/chat/ChatArea", () => ({
	ChatArea: (props: ChatHarnessProps) => {
		harness.chatProps = props;
		return <div data-testid="chat-area" />;
	},
}));

vi.mock("../src/components/SummaryPanel", () => ({
	SummaryPanel: (props: SummaryHarnessProps) => {
		harness.summaryProps = props;
		return <section data-testid="summary-panel" />;
	},
}));

vi.mock("../src/components/ModelsDialog", () => ({
	ModelsDialog: () => null,
}));

vi.mock("../src/components/SkillsDialog", () => ({
	SkillsDialog: () => null,
}));

vi.mock("../src/components/PluginsDialog", () => ({
	PluginsDialog: () => null,
}));

vi.mock("../src/components/ConfirmDialog", () => ({
	ConfirmDialog: () => null,
}));

vi.mock("../src/lib/chatBridge", async () => {
	const { catalogFixture } = await import("./catalogFixture");
	const bridge = {
		activateSession: vi.fn(async () => undefined),
		deleteSession: vi.fn(),
		getAgentRuntimeConfig: vi.fn(async () => ({
			selectableModels: [{ model: "test" }],
		})),
		listenAgentRuntimeConfigChanges: vi.fn(async () => () => undefined),
		listSessions: vi.fn(async () => []),
		updateSession: vi.fn(),
	};
	return {
		...bridge,
		querySessionCatalog: catalogFixture(bridge.listSessions),
	};
});

vi.mock("../src/lib/workspaceBridge", () => ({
	activateWorkspaceRoot: vi.fn(),
	getWorkspaceGitHubCliStatus: vi.fn(async () => ({
		available: false,
		summary: "",
	})),
	getWorkspaceGitStatus: vi.fn(async (workspaceRoot: string) => ({
		workspaceRoot,
		changedFiles: [],
		totalAdded: 0,
		totalRemoved: 0,
		isGitRepository: true,
	})),
	getWorkspaceInfo: vi.fn(async () => ({
		activeWorkspaceRoot: "D:\\Workspace",
		workspaces: [
			{
				root: "D:\\Workspace",
				name: "Workspace",
				sortOrder: 0,
				updatedAt: 1,
			},
		],
		cancelled: false,
	})),
	openWorkspaceFolder: vi.fn(),
	readDesktopFilePreview: vi.fn((path: string) => {
		let resolveRequest: (response: DesktopFilePreviewReadResponse) => void =
			() => {};
		const promise = new Promise<DesktopFilePreviewReadResponse>((resolve) => {
			resolveRequest = resolve;
		});
		harness.previewRequests.push({
			path,
			promise,
			resolve: resolveRequest,
		});
		return promise;
	}),
	resetWorkspaceCatalog: vi.fn(),
}));

import App from "../src/App";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const getChatProps = (): ChatHarnessProps => {
	if (!harness.chatProps) throw new Error("ChatArea has not rendered");
	return harness.chatProps;
};

const getSummaryProps = (): SummaryHarnessProps => {
	if (!harness.summaryProps) throw new Error("SummaryPanel has not rendered");
	return harness.summaryProps;
};

const preview = (
	path: string,
	content: string,
): DesktopFilePreviewReadResponse => ({
	root: "D:\\Workspace",
	path,
	name: path,
	content,
	byteLen: content.length,
	encoding: "utf-8",
	contentKind: "text",
	mimeType: "text/plain",
});

beforeEach(() => {
	vi.stubGlobal(
		"ResizeObserver",
		class {
			observe() {}
			unobserve() {}
			disconnect() {}
		},
	);
	vi.stubGlobal("matchMedia", () => ({
		matches: false,
		addEventListener() {},
		removeEventListener() {},
	}));
	harness.chatProps = null;
	harness.summaryProps = null;
	harness.previewRequests.length = 0;
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});
const click = async (name: string) => {
	await userEvent.setup().click(await screen.findByRole("button", { name }));
	if (name === "Workspace overview")
		await screen.findByRole("dialog", { name: "Workspace" });
};

test("chat controls belong to the chat column beside the detail pane", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	const chat = screen.getByRole("main");
	expect(
		within(chat).queryAllByRole("button", { name: "Workspace overview" }),
	).toHaveLength(1);
	await act(async () => getChatProps().onOpenAgentSession("agent", "Agent"));
	expect(
		within(chat).queryAllByRole("button", { name: "Workspace overview" }),
	).toHaveLength(1);
	expect(within(chat).queryAllByLabelText("Preview")).toHaveLength(0);
	await act(async () => renderer.unmount());
});

test("workspace resources dismiss outside and can be reopened", async () => {
	const user = userEvent.setup();
	render(<App />);
	await screen.findByTestId("chat-area");
	await user.click(screen.getByRole("button", { name: "Workspace overview" }));
	await screen.findByRole("dialog", { name: "Workspace" });
	await user.click(screen.getByTestId("chat-area"));
	await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
	await user.click(screen.getByRole("button", { name: "Workspace overview" }));
	expect(
		await screen.findByRole("button", { name: "Browse files" }),
	).toBeTruthy();
});

test("sidebar toggles preserve the open preview and its selected tab", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	await act(async () => getChatProps().onOpenAgentSession("agent-1", "Agent"));
	const before = getSummaryProps().tabs;
	await act(async () => click("Hide left sidebar"));
	expect(getSummaryProps().tabs).toEqual(before);
	expect(getSummaryProps().activeTabId).toBe("agent:agent-1");
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"false",
	);
	await act(async () => click("Show left sidebar"));
	expect(getSummaryProps().tabs).toEqual(before);
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"false",
	);
	await act(async () => renderer.unmount());
});

test("file browsing retains three files and replaces only the fourth preview slot", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	await act(async () => {
		await click("Workspace overview");
	});
	await act(async () => click("Browse files"));
	const operations: Promise<void>[] = [];
	await act(async () => {
		for (const path of ["a.md", "b.md", "c.md", "d.md", "e.md"]) {
			operations.push(getChatProps().onOpenWorkspacePath(path));
		}
	});
	expect(getSummaryProps().tabs.map((tab) => tab.title)).toEqual([
		"a.md",
		"b.md",
		"c.md",
		"e.md",
	]);
	expect(getSummaryProps().tabs.map((tab) => Boolean(tab.isPreview))).toEqual([
		false,
		false,
		false,
		true,
	]);
	await act(async () => {
		for (const request of [...harness.previewRequests].reverse())
			request.resolve(preview(request.path, request.path));
		await Promise.all(operations);
	});
	expect(getSummaryProps().tabs.map((tab) => tab.content)).toEqual([
		"a.md",
		"b.md",
		"c.md",
		"e.md",
	]);
	expect(getSummaryProps().tabs[3].isPreview).toBe(true);

	await act(async () => {
		await getChatProps().onOpenWorkspacePath("a.md");
	});
	expect(getSummaryProps().activeTabId).toBe(getSummaryProps().tabs[3].id);
	expect(getSummaryProps().tabs.map((tab) => tab.title)).toEqual([
		"e.md",
		"b.md",
		"c.md",
		"a.md",
	]);
	expect(harness.previewRequests).toHaveLength(5);
	// A freed retained slot is filled before the reusable preview; other surfaces are untouched.
	await act(async () =>
		getSummaryProps().onCloseTab(getSummaryProps().tabs[1].id),
	);
	await act(async () => {
		getChatProps().onOpenAgentSession("agent-1", "Agent");
		void getChatProps().onOpenWorkspacePath("f.md");
	});
	expect(
		getSummaryProps()
			.tabs.filter((tab) => tab.kind === "file")
			.map((tab) => tab.title),
	).toEqual(["e.md", "c.md", "f.md", "a.md"]);
	expect(
		getSummaryProps().tabs.find((tab) => tab.title === "Agent"),
	).toBeDefined();
	await act(async () => renderer.unmount());
});

test("workspace overview opens a reusable files content tab", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	const overview = screen.getByRole("button", { name: "Workspace overview" });
	await act(async () => userEvent.setup().click(overview));
	await act(async () => click("Browse files"));
	expect(getSummaryProps().tabs).toMatchObject([
		{ kind: "files", title: "Files" },
	]);
	await act(async () => userEvent.setup().click(overview));
	await act(async () => click("Browse files"));
	expect(getSummaryProps().tabs).toHaveLength(1);
	await act(async () => renderer.unmount());
});

test("opening the overview refreshes Git facts instead of keeping the initialization snapshot", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	vi.mocked(getWorkspaceGitStatus).mockResolvedValueOnce({
		workspaceRoot: "D:\\Workspace",
		branch: "new-branch",
		changedFiles: [],
		totalAdded: 12,
		totalRemoved: 1,
		isGitRepository: true,
	});
	await act(async () => click("Workspace overview"));
	expect(await screen.findByText("new-branch")).toBeTruthy();
	await act(async () => renderer.unmount());
});

test("closing a loading file does not let its late response reopen the tab", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	let operation!: Promise<void>;
	await act(async () => {
		operation = getChatProps().onOpenWorkspacePath("late.md");
	});
	const id = getSummaryProps().tabs[0].id;
	await act(async () => getSummaryProps().onCloseTab(id));
	await act(async () => {
		harness.previewRequests[0].resolve(preview("late.md", "late"));
		await operation;
	});
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"true",
	);
	await act(async () => renderer.unmount());
});

test("reopening a loaded file focuses its existing view without resetting it to loading", async () => {
	let renderer!: ReturnType<typeof render>;
	await act(async () => {
		renderer = render(<App />);
	});
	let operation!: Promise<void>;
	await act(async () => {
		operation = getChatProps().onOpenWorkspacePath("readme.md");
	});
	await act(async () => {
		harness.previewRequests[0].resolve(preview("readme.md", "readme"));
		await operation;
	});
	await act(async () => getSummaryProps().onCollapse());
	await act(async () => {
		void getChatProps().onOpenWorkspacePath("readme.md");
	});
	expect(harness.previewRequests).toHaveLength(1);
	expect(getSummaryProps().tabs).toMatchObject([
		{ content: "readme", loading: false },
	]);
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"false",
	);
	await act(async () => renderer.unmount());
});

test("the latest request owns a file preview when reads finish out of order", async () => {
	let renderer: ReturnType<typeof render> | null = null;
	await act(async () => {
		renderer = render(<App />);
	});

	let olderOperation: Promise<void> | null = null;
	let newerOperation: Promise<void> | null = null;
	await act(async () => {
		olderOperation = getChatProps().onOpenWorkspacePath("src/App.tsx", {
			startLine: 10,
		});
		newerOperation = getChatProps().onOpenWorkspacePath("src/App.tsx", {
			startLine: 20,
		});
	});
	expect(harness.previewRequests).toHaveLength(2);

	await act(async () => {
		harness.previewRequests[1].resolve(preview("src/App.tsx", "newer"));
		await newerOperation;
	});
	expect(getSummaryProps().tabs).toMatchObject([
		{ content: "newer", targetLine: 20, loading: false },
	]);

	await act(async () => {
		harness.previewRequests[0].resolve(preview("src/App.tsx", "older"));
		await olderOperation;
	});
	expect(getSummaryProps().tabs).toMatchObject([
		{ content: "newer", targetLine: 20, loading: false },
	]);

	await act(async () => renderer?.unmount());
});

test("Agent tabs are durable and closing the active tab selects its neighbor", async () => {
	let renderer: ReturnType<typeof render> | null = null;
	await act(async () => {
		renderer = render(<App />);
	});

	await act(async () => {
		getChatProps().onOpenAgentSession("agent-1", "First title");
		getChatProps().onOpenAgentSession("agent-1", "Second title");
		getChatProps().onOpenAgentSession("agent-2", "Another Agent");
	});

	expect(getSummaryProps().tabs).toMatchObject([
		{ id: "agent:agent-1", sessionId: "agent-1", title: "First title" },
		{ id: "agent:agent-2", sessionId: "agent-2", title: "Another Agent" },
	]);
	expect(getSummaryProps().activeTabId).toBe("agent:agent-2");
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"false",
	);

	await act(async () => getSummaryProps().onCloseTab("agent:agent-2"));
	expect(getSummaryProps().tabs).toHaveLength(1);
	expect(getSummaryProps().activeTabId).toBe("agent:agent-1");

	await act(async () => getSummaryProps().onCollapse());
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"true",
	);
	const showButton = screen.getByRole("button", { name: "Show right sidebar" });
	await act(async () => userEvent.setup().click(showButton));
	expect(screen.getByLabelText("Preview").getAttribute("aria-hidden")).toBe(
		"false",
	);

	await act(async () => renderer?.unmount());
});
